// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P6, the membership log cannot wedge: the room rule of `capacity.rs` and its laws, proved for
// every size by the kernel on every Trust build. The spec kernels are what capacity.rs's kernels
// compute; the World of section 3 asks the kernels as parameters, and
// `capacity_binding_island.rs` instantiates it with the compiled Rust. `capacity::tests`
// recomputes section 7's answer sheet from the Rust and reads its words from this file.
clean {
    -- The decision's scalar kernels. stop_judges and writer_waits, the contract!
    -- kernels of crates/aterm-mesh-core/src/capacity.rs, state `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves each
    -- compiled body equal to the definition here. keeps_room compares a computed
    -- integer, which E6 does not import yet (Trust's I2'), so section 7's sheet
    -- holds the Rust to spec_keeps_room until then; section 1 proves what
    -- spec_keeps_room means at every u64. They sit outside the namespace so a
    -- clause can name them.

    -- spec_keeps_room: a log holding `held` of `limit` takes `cost` more and keeps
    -- `need` free, written as the Rust is, with no sum that can wrap. Its meaning,
    -- `held + cost + need <= limit` over Nat, is section 1's keeps_room_means.
    def spec_keeps_room (held cost need limit : UInt64) : Bool :=
      decide (held ≤ limit) && (decide (cost ≤ limit - held) && decide (need ≤ limit - held - cost))
    -- spec_stop_judges: the 8,500 stop judges a re-key only where no held Remove
    -- made it due.
    def spec_stop_judges (rotation : Bool) : Bool := !rotation
    -- spec_writer_waits: a writer that waits for keys and minted nothing waits while
    -- the audience is due a rotation.
    def spec_writer_waits (waitsForKey mintedHere rotationDue : Bool) : Bool :=
      waitsForKey && (!mintedHere && rotationDue)

    namespace AtermMeshCapacity

    open AtermMeshTemporal

    -- ===========================================================================
    -- 1. What the kernels mean, and what the World asks of them.
    -- ===========================================================================

    -- The room the World reads: the sum over Nat, which never wraps.
    def roomNat (held cost need limit : Nat) : Bool := decide (held + cost + need ≤ limit)

    -- spec_keeps_room means roomNat at every u64: keeps_room_means below.
    def KeepsRoomMeans : Prop :=
      ∀ (h c n l : UInt64), spec_keeps_room h c n l = roomNat h.toNat c.toNat n.toNat l.toNat

    -- Its proof. Clean's core has no lemma for UInt64 subtraction's Nat meaning
    -- and its omega reads no truncated subtraction, so the few Nat facts it needs
    -- are proved here from Nat.div_add_mod, Nat.mod_lt and the successor cases:
    -- `(l - h).toNat` is `(l.toNat + (2^64 - h.toNat)) % 2^64` by definition, which
    -- is `l.toNat - h.toNat` once `h <= l` (kr_toNat_sub), and each subtraction of
    -- spec_keeps_room is read only past the comparison that keeps it from wrapping.
    theorem kr_sub_add_cancel (m : Nat) : (n : Nat) → m ≤ n → n - m + m = n := by
      induction m with
      | zero => intro n _; rfl
      | succ m ih =>
        intro n h
        cases n with
        | zero => exact False.elim (Nat.not_succ_le_zero m h)
        | succ n =>
          rw [Nat.succ_sub_succ n m]
          show Nat.succ (n - m + m) = Nat.succ n
          rw [ih n (Nat.le_of_succ_le_succ m n h)]

    theorem kr_le_sub_of_add_le (a b c : Nat) (h : a + b ≤ c) : a ≤ c - b := by
      have h2 := Nat.sub_le_sub_right (a + b) c b h
      rw [Nat.add_sub_cancel a b] at h2
      exact h2

    theorem kr_add_le_of_le_sub (a b c : Nat) (hb : b ≤ c) (h : a ≤ c - b) : a + b ≤ c := by
      have h2 := Nat.add_le_add_right a (c - b) h b
      rw [kr_sub_add_cancel b c hb] at h2
      exact h2

    theorem kr_le_rw (a b c : Nat) (h : a ≤ b) (e : b = c) : a ≤ c := by
      rw [← e]
      exact h

    theorem kr_lin2 (t S m r : Nat) (h : S + t + m = r + S) (ht : S ≤ t) (hr : r < S) : False :=
      have h1 : (t + m) + S = r + S :=
        Eq.trans (Nat.add_comm (t + m) S) (Eq.trans (Nat.add_assoc S t m).symm h)
      have h2 : t + m = r := Nat.add_right_cancel (t + m) S r h1
      have h3 : t ≤ r := kr_le_rw t (t + m) r (Nat.le_add_right t m) h2
      Nat.lt_irrefl r (Nat.lt_of_lt_of_le r S r hr (Nat.le_trans S t r ht h3))

    theorem kr_mod_unique (q m r S : Nat) (h : q * S + m = r + S) (hm : m < S) (hr : r < S) : m = r := by
      cases q with
      | zero =>
        have h0 : m = r + S :=
          Eq.trans (Eq.trans (Nat.zero_add m).symm (congrArg (fun x => x + m) (Nat.zero_mul S)).symm) h
        have hSm : S ≤ m := kr_le_rw S (r + S) m (Nat.le_add_left S r) h0.symm
        exact False.elim (Nat.lt_irrefl m (Nat.lt_of_lt_of_le m S m hm hSm))
      | succ q =>
        have h1 : S + q * S + m = r + S :=
          Eq.trans (congrArg (fun x => x + m) (Nat.succ_mul q S)).symm h
        cases q with
        | zero =>
          have h2 : S + m = S + r :=
            Eq.trans (Eq.trans (congrArg (fun x => x + m) (Eq.trans (congrArg (fun x => S + x) (Nat.zero_mul S)) (Nat.add_zero S))).symm h1) (Nat.add_comm r S)
          exact Nat.add_left_cancel S m r h2
        | succ q =>
          have ht : S ≤ Nat.succ q * S :=
            kr_le_rw S (S + q * S) (Nat.succ q * S) (Nat.le_add_right S (q * S)) (Nat.succ_mul q S).symm
          exact False.elim (kr_lin2 (Nat.succ q * S) S m r h1 ht hr)

    theorem kr_add_mod_self (r S : Nat) (hr : r < S) : (r + S) % S = r :=
      have hS : 0 < S := Nat.lt_of_le_of_lt 0 r S (Nat.zero_le r) hr
      kr_mod_unique ((r + S) / S) ((r + S) % S) r S (Nat.div_add_mod (r + S) S) (Nat.mod_lt (r + S) S hS) hr

    theorem kr_lin3 (x a b y S : Nat) (ha : a + y = x) (hb : b + y = S) : x + b = a + S := by
      rw [← ha, ← hb]
      rw [Nat.add_assoc a y b, Nat.add_comm y b]

    theorem kr_sub_mod (x y S : Nat) (hy : y ≤ x) (hx : x < S) : (x + (S - y)) % S = x - y :=
      have hyS : y ≤ S := Nat.le_of_lt y S (Nat.lt_of_le_of_lt y x S hy hx)
      have e : x + (S - y) = (x - y) + S := kr_lin3 x (x - y) (S - y) y S (kr_sub_add_cancel y x hy) (kr_sub_add_cancel y S hyS)
      have hlt : x - y < S := Nat.lt_of_le_of_lt (x - y) x S (Nat.sub_le x y) hx
      Eq.trans (congrArg (fun z => z % S) e) (kr_add_mod_self (x - y) S hlt)

    theorem kr_toNat_sub_def (a b : UInt64) : (a - b).toNat = (a.toNat + (UInt64.size - b.toNat)) % UInt64.size := rfl

    theorem kr_toNat_sub (a b : UInt64) (h : b.toNat ≤ a.toNat) : (a - b).toNat = a.toNat - b.toNat :=
      Eq.trans (kr_toNat_sub_def a b) (kr_sub_mod a.toNat b.toNat UInt64.size h (UInt64.toNat_lt a))

    theorem kr_decide_true (p : Prop) [inst : Decidable p] (hp : p) : decide p = true :=
      match inst with
      | isTrue _ => rfl
      | isFalse hn => absurd hp hn

    theorem kr_decide_false (p : Prop) [inst : Decidable p] (hp : ¬p) : decide p = false :=
      match inst with
      | isTrue hy => absurd hy hp
      | isFalse _ => rfl

    theorem kr_rev3 (a b c : Nat) : a + b + c = c + b + a := by
      rw [Nat.add_comm a b, Nat.add_comm (b + a) c, ← Nat.add_assoc c b a]

    theorem kr_le_rw_l (a b c : Nat) (h : b ≤ c) (e : a = b) : a ≤ c := by
      rw [e]
      exact h

    theorem kr_room_of (H C N L : Nat) (h1 : H ≤ L) (h2 : C ≤ L - H) (h3 : N ≤ L - H - C) :
        H + C + N ≤ L :=
      kr_le_rw_l (H + C + N) (N + C + H) L
        (kr_add_le_of_le_sub (N + C) H L h1 (kr_add_le_of_le_sub N C (L - H) h2 h3)) (kr_rev3 H C N)

    theorem kr_room_split (H C N L : Nat) (hr : H + C + N ≤ L) : N + C ≤ L - H :=
      kr_le_sub_of_add_le (N + C) H L (kr_le_rw_l (N + C + H) (H + C + N) L hr (kr_rev3 H C N).symm)

    theorem kr_room_n (H C N L : Nat) (hr : H + C + N ≤ L) : N ≤ L - H - C :=
      kr_le_sub_of_add_le N C (L - H) (kr_room_split H C N L hr)

    theorem kr_room_c (H C N L : Nat) (hr : H + C + N ≤ L) : C ≤ L - H :=
      Nat.le_trans C (N + C) (L - H) (Nat.le_add_left C N) (kr_room_split H C N L hr)

    theorem kr_room_h (H C N L : Nat) (hr : H + C + N ≤ L) : H ≤ L :=
      Nat.le_trans H (H + C + N) L
        (Nat.le_trans H (H + C) (H + C + N) (Nat.le_add_right H C) (Nat.le_add_right (H + C) N)) hr

    theorem kr_means_at (h c n l : UInt64) :
        spec_keeps_room h c n l = roomNat h.toNat c.toNat n.toNat l.toNat := by
      show (decide (h ≤ l) && (decide (c ≤ l - h) && decide (n ≤ l - h - c))) =
        decide (h.toNat + c.toNat + n.toNat ≤ l.toNat)
      by_cases h1 : h.toNat ≤ l.toNat
      · have e1 : (l - h).toNat = l.toNat - h.toNat := kr_toNat_sub l h h1
        rw [kr_decide_true (h ≤ l) h1]
        by_cases h2 : c.toNat ≤ l.toNat - h.toNat
        · have h2' : c.toNat ≤ (l - h).toNat := kr_le_rw c.toNat (l.toNat - h.toNat) (l - h).toNat h2 e1.symm
          have e2 : (l - h - c).toNat = l.toNat - h.toNat - c.toNat :=
            Eq.trans (kr_toNat_sub (l - h) c h2') (congrArg (fun x => x - c.toNat) e1)
          rw [kr_decide_true (c ≤ l - h) h2']
          by_cases h3 : n.toNat ≤ l.toNat - h.toNat - c.toNat
          · have h3' : n.toNat ≤ (l - h - c).toNat :=
              kr_le_rw n.toNat (l.toNat - h.toNat - c.toNat) (l - h - c).toNat h3 e2.symm
            rw [kr_decide_true (n ≤ l - h - c) h3',
              kr_decide_true (h.toNat + c.toNat + n.toNat ≤ l.toNat) (kr_room_of h.toNat c.toNat n.toNat l.toNat h1 h2 h3)]
          · have h3' : ¬ (n.toNat ≤ (l - h - c).toNat) := fun hn =>
              h3 (kr_le_rw n.toNat (l - h - c).toNat (l.toNat - h.toNat - c.toNat) hn e2)
            rw [kr_decide_false (n ≤ l - h - c) h3',
              kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h3 (kr_room_n h.toNat c.toNat n.toNat l.toNat hr))]
        · have h2' : ¬ (c.toNat ≤ (l - h).toNat) := fun hc =>
            h2 (kr_le_rw c.toNat (l - h).toNat (l.toNat - h.toNat) hc e1)
          rw [kr_decide_false (c ≤ l - h) h2',
            kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h2 (kr_room_c h.toNat c.toNat n.toNat l.toNat hr))]
      · rw [kr_decide_false (h ≤ l) h1,
          kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h1 (kr_room_h h.toNat c.toNat n.toNat l.toNat hr))]

    theorem keeps_room_means : KeepsRoomMeans := kr_means_at

    -- Exactness: what a kernel the World asks must compute. The binding island
    -- proves it of the compiled stop and wait.
    def RoomExact (room : Nat → Nat → Nat → Nat → Bool) : Prop :=
      ∀ h c n l, room h c n l = roomNat h c n l
    def JudgesExact (judges : Bool → Bool) : Prop := ∀ r, judges r = spec_stop_judges r
    def WaitsExact (waits : Bool → Bool → Bool → Bool) : Prop :=
      ∀ k m d, waits k m d = spec_writer_waits k m d

    theorem room_true (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) (h c n l : Nat)
        (hok : room h c n l = true) : h + c + n ≤ l :=
      of_decide_eq_true (Eq.trans (hR h c n l).symm hok)

    theorem and_right (a b : Bool) (h : (a && b) = true) : b = true :=
      (Eq.mp (Bool.and_eq_true a b) h).2

    -- A writer waits only on a re-key the stop never judges: both read one
    -- rotation-due fact (`wraps_removed`).
    theorem writer_waits_only_on_exempt (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (hW : WaitsExact waits)
        (k m due : Bool) (h : waits k m due = true) : judges due = false := by
      rw [hJ due]
      rw [hW k m due] at h
      cases due with
      | true => rfl
      | false => cases k <;> cases m <;> exact Bool.noConfusion h

    -- Plain-variable arithmetic (Clean's omega reads no structure projection).
    theorem ar_reserve (k c a n L : Nat) (h1 : c + a ≤ n) (h2 : k + n ≤ L) : k + c + a ≤ L := by
      omega
    theorem ar_fits (k c a n L : Nat) (h1 : c + a ≤ n) (h2 : k + n ≤ L) : k + c ≤ L := by omega
    theorem ar_auto (k r c a n L : Nat) (h : k + r + c + n ≤ L) (ha : a = n) : k + c + a ≤ L := by
      omega
    theorem ar_init (k n L : Nat) (h1 : k = 0) (h2 : n = 0) : k + n ≤ L := by omega
    theorem ar_prune (k j n L : Nat) (hj : j ≤ k) (h : k + n ≤ L) : j + n ≤ L := by omega
    theorem ar_remove (k b a x L : Nat) (hc : a ≤ x) (h : k + b + x ≤ L) : k + b + a ≤ L := by
      omega

    -- ===========================================================================
    -- 2. The production constants (budgets.rs, readmit.rs).
    -- ===========================================================================

    def stopOf (keyedBudget : Nat) : Nat := keyedBudget * 8500 / 12288
    def proofLine (keyedBudget : Nat) : Nat := (keyedBudget * 3 + 3) / 4
    theorem stop_production : stopOf 12288 = 8500 := by decide
    theorem proof_line_production : proofLine 12288 = 9216 := by decide
    theorem stop_below_proof_line_production : stopOf 12288 < proofLine 12288 := by decide

    -- ===========================================================================
    -- 3. The World: one Mac's log in one dimension, records or bytes, at every
    --    size.
    -- ===========================================================================

    -- L is the bound (RecordBudgets::keyed, 12,288 records, or held_bytes_bound);
    -- S the tender's stop (8,500 records; the bound itself for bytes, which have
    -- none); KR the proof-admission line (9,216 records; past the bound for bytes).
    structure Budgets where
      L : Nat
      S : Nat
      KR : Nat

    def records : Budgets := Budgets.mk 12288 8500 9216
    def bytesOf (bound : Nat) : Budgets := Budgets.mk bound bound (bound + 1)

    -- keyed: the log's keyed records (Log::counts) or wire bytes (Log::held_bytes);
    -- need: what the re-keys a held Remove made due owe at worst, of the dues some
    -- Mac can sign (log_entries.rs owed_rekeys over owed_audiences: a due no Mac
    -- carries is never signed, so no fill refuses it); stuck: a re-key writers wait
    -- on was refused at a fill that only grows, the wedge.
    structure World where
      keyed : Nat
      need : Nat
      stuck : Bool

    -- One keyed write, its cost and the need after it.
    inductive Event where
      | tend (rotation : Bool) (ready cost after : Nat) -- tend_epochs_at, one due audience, `ready` kept before it
      | write (rotationDue : Bool) (cost after : Nat) -- a person's [everyone] Put
      | prove (cost after : Nat) -- readmit's decide: an Admit and its Wraps
      | heal (cost : Nat) -- close_wrap_gaps: one Wrap
      | person (cost after : Nat) -- any other keyed write a person makes
      | remove (batch worst after : Nat) -- remove_member with its RemovalFill
      | prune (kept : Nat) -- D8: a final checkpoint keeps `kept`

    -- Faithfulness: what the Rust computes about an event, held by tests, not
    -- proved. A due re-key releases at least its own cost from need (rekey_bound is
    -- never below what a re-key signs: remove_tests'
    -- the_planned_cost_of_an_audience_not_opened_whole_counts_no_superseded_write,
    -- record_budgets' a_rekeys_records_take_their_most_bytes); a re-key no removal
    -- made is of an audience no held Remove owes, so it changes no due; a removal's
    -- worst case covers every re-key owed after it (capacity_tests'
    -- a_removal_reserves_the_rekeys_an_earlier_removal_made_due and
    -- a_removal_reserves_its_rekeys_bytes); a checkpoint only drops records.
    def WF (w : World) : Event → Prop
      | Event.tend true _ cost after => cost + after ≤ w.need
      | Event.tend false _ _ after => after = w.need
      | Event.remove _ worst after => after ≤ worst
      | Event.prune kept => kept ≤ w.keyed
      | _ => True

    -- One batch, all or none: signed (`ok`), or refused; a refused due re-key
    -- wedges its writers.
    def sign (w : World) (ok : Bool) (cost after : Nat) (rot : Bool) : World :=
      match ok with
      | true => World.mk (w.keyed + cost) after w.stuck
      | false => World.mk w.keyed w.need (w.stuck || rot)

    -- THE RULE AS SHIPPED, over the kernels it asks: the tender
    -- (within_automatic_stop: the stop, and the owed room, for a re-key the stop
    -- judges), the writer's wait (waits_in), a heal (heal_room) and a removal
    -- (RemovalFill) through room; a re-key a held Remove made due, a proof admission
    -- and a person's other writes through Log::insert's hard bound alone.
    def stepNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w : World) : Event → World
      | Event.tend rot ready cost after =>
          match judges rot with
          | true =>
              sign w (room (w.keyed + ready) cost 0 b.S && room (w.keyed + ready) cost w.need b.L)
                cost after rot
          | false => sign w (decide (w.keyed + cost ≤ b.L)) cost after rot
      | Event.write due cost after =>
          match waits true false due with
          | true => w
          | false => sign w (decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.prove cost after =>
          sign w (decide (w.keyed < b.KR) && decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.heal cost => sign w (room w.keyed cost w.need b.L) cost w.need false
      | Event.person cost after => sign w (decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.remove batch worst after => sign w (room w.keyed batch worst b.L) batch after false
      | Event.prune kept => World.mk kept w.need w.stuck

    -- THE RULE P6 PROPOSES for what the shipped one leaves (F3, F4): a proof
    -- admission and a person's writes keep need free too.
    def stepFix (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w : World) : Event → World
      | Event.write due cost after =>
          match waits true false due with
          | true => w
          | false => sign w (room w.keyed cost after b.L) cost after false
      | Event.prove cost after =>
          sign w (decide (w.keyed < b.KR) && room w.keyed cost after b.L) cost after false
      | Event.person cost after => sign w (room w.keyed cost after b.L) cost after false
      | e => stepNow b judges waits room w e

    def Inv (b : Budgets) (w : World) : Prop := w.keyed + w.need ≤ b.L ∧ w.stuck = false

    -- ===========================================================================
    -- 4. The shipped rule never wedges under the premises it leaves.
    -- ===========================================================================

    -- The stop holds no due re-key: its step reads no S, at any fill.
    theorem tend_exempt (b : Budgets) (judges : Bool → Bool) (hJ : judges true = false)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (w : World) (ready cost after : Nat) :
        stepNow b judges waits room w (Event.tend true ready cost after) =
          sign w (decide (w.keyed + cost ≤ b.L)) cost after true := by
      simp only [stepNow, hJ]

    theorem sign_inv (b : Budgets) (w : World) (ok : Bool) (cost after : Nat) (rot : Bool)
        (hi : Inv b w) (hok : ok = true → w.keyed + cost + after ≤ b.L)
        (hrot : ok = false → rot = false) : Inv b (sign w ok cost after rot) := by
      cases ok with
      | true => exact And.intro (hok rfl) hi.2
      | false =>
        have hr : rot = false := hrot rfl
        exact And.intro hi.1 (by show (w.stuck || rot) = false; rw [hi.2, hr])

    -- THE PREMISES THE SHIPPED RULE LEAVES (Calm), each in records and in bytes: a
    -- person's [everyone] Put and other writes (F4) and a proof admission (F3) keep
    -- need free, which they do not check. F1, F2, F6 and a heal's F4 need none.
    def Calm (b : Budgets) (w : World) : Event → Prop
      | Event.write _ cost after => w.keyed + cost + after ≤ b.L
      | Event.prove cost after => w.keyed + cost + after ≤ b.L
      | Event.person cost after => w.keyed + cost + after ≤ b.L
      | _ => True

    theorem now_preserves_when_calm (b : Budgets) (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) (w : World) (e : Event) (hwf : WF w e) (hc : Calm b w e)
        (hi : Inv b w) : Inv b (stepNow b judges waits room w e) := by
      have hJt : judges true = false := hJ true
      have hJf : judges false = true := hJ false
      cases e with
      | tend rot ready cost after =>
        cases rot with
        | true =>
          rw [tend_exempt b judges hJt waits room w ready cost after]
          have hw : cost + after ≤ w.need := hwf
          exact sign_inv b w _ cost after true hi
            (fun _ => ar_reserve w.keyed cost after w.need b.L hw hi.1)
            (fun h => absurd (ar_fits w.keyed cost after w.need b.L hw hi.1) (of_decide_eq_false h))
        | false =>
          have hw : after = w.need := hwf
          simp only [stepNow, hJf]
          exact sign_inv b w _ cost after false hi
            (fun h => ar_auto w.keyed ready cost after w.need b.L
              (room_true room hR _ _ _ _ (and_right _ _ h)) hw)
            (fun _ => rfl)
      | write due cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        cases hd : waits true false due with
        | true =>
          simp only [stepNow, hd]
          exact hi
        | false =>
          simp only [stepNow, hd]
          exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | prove cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | heal cost =>
        exact sign_inv b w _ cost w.need false hi (fun h => room_true room hR _ _ _ _ h)
          (fun _ => rfl)
      | person cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | remove batch worst after =>
        have hw : after ≤ worst := hwf
        exact sign_inv b w _ batch after false hi
          (fun h => ar_remove w.keyed batch after worst b.L hw (room_true room hR _ _ _ _ h))
          (fun _ => rfl)
      | prune kept =>
        exact And.intro (ar_prune w.keyed kept w.need b.L hwf hi.1) hi.2

    def Init (w : World) : Prop := w.keyed = 0 ∧ w.need = 0 ∧ w.stuck = false
    def NextNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w w' : World) : Prop :=
      ∃ e, WF w e ∧ Calm b w e ∧ w' = stepNow b judges waits room w e
    def SpecNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) : StateMachine World :=
      StateMachine.mk Init (NextNow b judges waits room)

    -- Spec |= [](keyed + need <= L and not stuck) for the rule as shipped, over a
    -- stop and a room exact to their specs and any writer's wait (a writer that waits
    -- writes nothing, and one that writes is Calm), for every behaviour whose steps
    -- are faithful and calm, at every size and in either dimension.
    theorem spec_now_never_wedges_when_calm (b : Budgets) (judges : Bool → Bool)
        (hJ : JudgesExact judges) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) :
        Satisfies (SpecNow b judges waits room) (Always (Lift (Inv b))) :=
      invariantSatisfies (SpecNow b judges waits room) (Inv b)
        (fun w h => And.intro (ar_init w.keyed w.need b.L h.1 h.2.1) h.2.2)
        (fun w w' hi hn => Exists.elim hn (fun e he =>
          Eq.mpr (congrArg (Inv b) he.2.2)
            (now_preserves_when_calm b judges hJ waits room hR w e he.1 he.2.1 hi)))

    -- The two readings the code runs: keyed records, and wire bytes, where the
    -- tender's bytes have no stop and a proof admission no line of their own.
    theorem records_never_wedge_when_calm (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) :
        Satisfies (SpecNow records judges waits room) (Always (Lift (Inv records))) :=
      spec_now_never_wedges_when_calm records judges hJ waits room hR
    theorem bytes_never_wedge_when_calm (bound : Nat) (judges : Bool → Bool)
        (hJ : JudgesExact judges) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) :
        Satisfies (SpecNow (bytesOf bound) judges waits room) (Always (Lift (Inv (bytesOf bound)))) :=
      spec_now_never_wedges_when_calm (bytesOf bound) judges hJ waits room hR

    -- ===========================================================================
    -- 5. The proposed rule needs no Calm.
    -- ===========================================================================

    theorem fix_preserves (b : Budgets) (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) (w : World) (e : Event) (hwf : WF w e) (hi : Inv b w) :
        Inv b (stepFix b judges waits room w e) := by
      cases e with
      | write due cost after =>
        cases hd : waits true false due with
        | true =>
          simp only [stepFix, hd]
          exact hi
        | false =>
          simp only [stepFix, hd]
          exact sign_inv b w _ cost after false hi (fun h => room_true room hR _ _ _ _ h)
            (fun _ => rfl)
      | prove cost after =>
        exact sign_inv b w _ cost after false hi
          (fun h => room_true room hR _ _ _ _ (and_right _ _ h)) (fun _ => rfl)
      | person cost after =>
        exact sign_inv b w _ cost after false hi (fun h => room_true room hR _ _ _ _ h)
          (fun _ => rfl)
      | tend rot ready cost after =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.tend rot ready cost after)
          hwf True.intro hi
      | heal cost =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.heal cost) hwf True.intro hi
      | remove batch worst after =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.remove batch worst after)
          hwf True.intro hi
      | prune kept =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.prune kept) hwf True.intro hi

    -- ===========================================================================
    -- 6. Counterexamples the kernel evaluates: each rule before PF6 (the mutants
    --    the Rust tests run as controls) wedges where the shipped rule holds, and
    --    the shipped rule wedges without each Calm premise.
    -- ===========================================================================

    def run (step : World → Event → World) (w : World) : List Event → World
      | [] => w
      | e :: es => run step (step w e) es
    def nowIn (b : Budgets) (judges : Bool → Bool) : World → Event → World :=
      stepNow b judges spec_writer_waits roomNat
    def now : World → Event → World := nowIn records spec_stop_judges
    def fix : World → Event → World := stepFix records spec_stop_judges spec_writer_waits roomNat

    -- HEAL_IGNORES_NEED: a heal weighs only the hard bound (F4). At keyed + need = L
    -- it takes a Wrap of the reserve, and the second due re-key is refused.
    def healIgnoresNeed (w : World) : Event → World
      | Event.heal cost => sign w (decide (w.keyed + cost ≤ records.L)) cost w.need false
      | e => now w e
    theorem heal_ignoring_need_takes_the_reserve :
        (run healIgnoresNeed (World.mk 12000 288 false)
          [Event.heal 1, Event.tend true 0 144 144, Event.tend true 0 144 0]).stuck = true := by
      decide
    theorem shipped_heal_keeps_the_reserve :
        (run now (World.mk 12000 288 false)
          [Event.heal 1, Event.tend true 0 144 144, Event.tend true 0 144 0]).stuck = false := by
      decide

    -- FILL_OWN_TARGET_ONLY: a second removal whose worst case counts its own target
    -- alone (142) takes the first one's reserve (F1); counting every owed re-key
    -- (426), as RemovalFill does, it refuses, and the first one's re-keys fit.
    theorem own_target_fill_takes_the_first_reserve :
        (run now (World.mk 11800 0 false)
          [Event.remove 0 284 284, Event.remove 142 142 426, Event.tend true 0 142 284,
           Event.tend true 0 142 142, Event.tend true 0 142 0]).stuck = true := by decide
    theorem shipped_second_removal_refuses :
        (run now (World.mk 11800 0 false)
          [Event.remove 0 284 284, Event.remove 142 426 426, Event.tend true 0 142 142,
           Event.tend true 0 142 0]).stuck = false := by decide

    -- STOP_HOLDS_REMOVALS: the stop judges every re-key. At 8,600 keyed a removal's
    -- 142-record rotation is held, and every writer waits for good.
    theorem stop_holding_removals_wedges :
        (run (nowIn records (fun _ => true)) (World.mk 8600 142 false)
          [Event.tend true 0 142 0]).stuck = true := by decide
    theorem exemption_signs_it :
        (run now (World.mk 8600 142 false) [Event.tend true 0 142 0]).stuck = false := by decide

    -- AUTOMATIC_IGNORES_OWED: the tender weighs the stop alone (F2). A large lab
    -- (about 4,000 seats): the removal reserves 8,000, a coalesce no removal made
    -- fills to the stop, and the rotation no longer fits.
    def tenderIgnoresOwed (w : World) : Event → World
      | Event.tend false ready cost after =>
          sign w (roomNat (w.keyed + ready) cost 0 records.S) cost after false
      | e => now w e
    theorem tender_ignoring_owed_wedges_a_large_lab :
        (run tenderIgnoresOwed (World.mk 4000 0 false)
          [Event.remove 0 8000 8000, Event.tend false 0 4500 8000,
           Event.tend true 0 4000 4000]).stuck = true := by decide
    theorem shipped_tender_holds_it :
        (run now (World.mk 4000 0 false)
          [Event.remove 0 8000 8000, Event.tend false 0 4500 8000,
           Event.tend true 0 4000 4000]).stuck = false := by decide

    -- FILL_IGNORES_BYTES: in the byte reading (a bound of 1,000 bytes), a removal
    -- that weighs records alone signs a batch whose re-keys' 600 bytes no longer fit
    -- (F6); the shipped fill refuses it, signing nothing.
    def bytes : Budgets := bytesOf 1000
    def fillIgnoresBytes (w : World) : Event → World
      | Event.remove batch _ after => sign w (decide (w.keyed + batch ≤ bytes.L)) batch after false
      | e => nowIn bytes spec_stop_judges w e
    theorem fill_ignoring_bytes_wedges :
        (run fillIgnoresBytes (World.mk 600 0 false)
          [Event.remove 10 600 600, Event.tend true 0 300 300, Event.tend true 0 300 0]).stuck
          = true := by decide
    theorem shipped_fill_refuses_the_bytes :
        (run (nowIn bytes spec_stop_judges) (World.mk 600 0 false) [Event.remove 10 600 600]).keyed
          = 600 := by decide

    -- WITHOUT CALM, F3: proof admissions below the 9,216 line take the room the
    -- due re-keys owe; the proposed rule refuses the admission.
    theorem shipped_rule_lets_a_proof_admission_take_the_reserve :
        (run now (World.mk 9000 3000 false)
          [Event.prove 400 3000, Event.tend true 0 1500 1500, Event.tend true 0 1500 0]).stuck
          = true := by decide
    theorem proposed_rule_refuses_the_admission :
        (run fix (World.mk 9000 3000 false)
          [Event.prove 400 3000, Event.tend true 0 1500 1500, Event.tend true 0 1500 0]).stuck
          = false := by decide

    -- WITHOUT CALM, F4: a person's 200 other writes take the room at 12,000.
    theorem shipped_rule_lets_person_writes_take_the_reserve :
        (run now (World.mk 12000 0 false)
          [Event.remove 0 284 284, Event.person 200 284, Event.tend true 0 142 142]).stuck
          = true := by decide
    theorem proposed_rule_refuses_the_person_write :
        (run fix (World.mk 12000 0 false)
          [Event.remove 0 284 284, Event.person 200 284, Event.tend true 0 142 142]).stuck
          = false := by decide

    -- ===========================================================================
    -- 7. The answer sheet: the three kernels on a grid, one bit each. Bits 0 to
    --    1295 are spec_keeps_room over every held, cost, need and limit in
    --    0, 1, 2, 3, 2^63, 2^64 - 1 (limit fastest); 1296 and 1297
    --    spec_stop_judges of false and true; 1298 to 1305 spec_writer_waits over
    --    its three Bools (the last fastest); the rest of the last word is zero.
    --    Sixty-four bits to a word (Clean's numerals fit 64 bits), the lowest bit
    --    index highest. The Rust test `capacity::tests::answer_sheet_matches_the_island`
    --    recomputes it from the Rust kernels and reads the words below from this
    --    file, which holds the Rust keeps_room to spec_keeps_room on the grid until
    --    E6 imports it (Trust's I2').
    -- ===========================================================================

    def gridValue : Nat → UInt64
      | 0 => 0
      | 1 => 1
      | 2 => 2
      | 3 => 3
      | 4 => 9223372036854775808
      | _ => 18446744073709551615

    def gridBool (i : Nat) : Bool := decide (i % 2 = 1)

    def bitWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (i : Nat) : Bool :=
      if i < 1296 then
        room (gridValue (i / 216)) (gridValue ((i / 36) % 6)) (gridValue ((i / 6) % 6))
          (gridValue (i % 6))
      else if i < 1298 then spec_stop_judges (gridBool (i - 1296))
      else if i < 1306 then
        spec_writer_waits (gridBool ((i - 1298) / 4)) (gridBool ((i - 1298) / 2))
          (gridBool (i - 1298))
      else false

    def wordWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 2 * wordWith room w n + (if bitWith room (64 * w + n) then 1 else 0)

    def sheetWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => wordWith room w 64 :: sheetWith room (w + 1) n

    -- Bits 0 to 1343, as 21 words.
    def sheet : List Nat := sheetWith spec_keeps_room 0 21

    -- Reuse the all-values meaning proof: the table need not re-evaluate
    -- modular UInt64 subtraction. This is the same sheet and the same grid,
    -- related to a Nat-only computation by a kernel-checked function equality.
    def room_as_nat (h c n l : UInt64) : Bool := roomNat h.toNat c.toNat n.toNat l.toNat

    theorem room_as_nat_is_exact : spec_keeps_room = room_as_nat := by
      funext h c n l
      exact keeps_room_means h c n l

    theorem sheet_reads_nat : sheet = sheetWith room_as_nat 0 21 :=
      congrArg (fun room : UInt64 → UInt64 → UInt64 → UInt64 → Bool => sheetWith room 0 21)
        room_as_nat_is_exact

    theorem nat_sheet_cons {a b : Nat} {xs ys : List Nat}
        (ha : a = b) (ht : xs = ys) : a :: xs = b :: ys :=
      Eq.trans (congrArg (fun x : Nat => x :: xs) ha)
        (congrArg (fun rest : List Nat => b :: rest) ht)

    -- Read the UInt64 grid once through a generic equality. The finite
    -- calculation then uses Nat literals, not 1344 repeated carrier projections.
    def gridNat : Nat → Nat
      | 0 => 0
      | 1 => 1
      | 2 => 2
      | 3 => 3
      | 4 => 9223372036854775808
      | _ => 18446744073709551615

    theorem gridValue_toNat (i : Nat) : (gridValue i).toNat = gridNat i := by
      cases i with
      | zero => rfl
      | succ i =>
        cases i with
        | zero => rfl
        | succ i =>
          cases i with
          | zero => rfl
          | succ i =>
            cases i with
            | zero => rfl
            | succ i =>
              cases i with
              | zero => rfl
              | succ i => rfl

    def natBit (i : Nat) : Bool :=
      if i < 1296 then
        roomNat (gridNat (i / 216)) (gridNat ((i / 36) % 6)) (gridNat ((i / 6) % 6))
          (gridNat (i % 6))
      else if i < 1298 then spec_stop_judges (gridBool (i - 1296))
      else if i < 1306 then
        spec_writer_waits (gridBool ((i - 1298) / 4)) (gridBool ((i - 1298) / 2))
          (gridBool (i - 1298))
      else false

    theorem bit_reads_nat : bitWith room_as_nat = natBit := by
      funext i
      unfold bitWith natBit room_as_nat
      rw [gridValue_toNat, gridValue_toNat, gridValue_toNat, gridValue_toNat]

    def natWord (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 2 * natWord w n + (if natBit (64 * w + n) then 1 else 0)

    theorem word_reads_nat (w n : Nat) : wordWith room_as_nat w n = natWord w n := by
      induction n with
      | zero => rfl
      | succ n ih =>
        change 2 * wordWith room_as_nat w n + (if bitWith room_as_nat (64 * w + n) then 1 else 0) =
          2 * natWord w n + (if natBit (64 * w + n) then 1 else 0)
        rw [ih, congrFun bit_reads_nat (64 * w + n)]
        rfl

    -- A word is assembled from checked bit decisions by congruence, rather
    -- than asking conversion to normalize its recursive prefix repeatedly.
    theorem natWord_step (w n value : Nat) (b : Bool)
        (hprefix : natWord w n = value) (bit : natBit (64 * w + n) = b) :
        natWord w (Nat.succ n) = 2 * value + (if b then 1 else 0) := by
      change 2 * natWord w n + (if natBit (64 * w + n) then 1 else 0) =
        2 * value + (if b then 1 else 0)
      rw [hprefix, bit]

    -- These closed proof terms read the bits of the unchanged answer words.
    -- Each `by decide` checks the corresponding natBit against the actual grid;
    -- natWord_step combines that fact with the prior prefix. The kernel checks
    -- every bit and every arithmetic prefix, including all padding bits.
    theorem nat_word_0_calculated : natWord 0 64 = 0xfdf3c70c17cf1c30 :=
      (natWord_step 0 63 0x7ef9e3860be78e18 false
        (natWord_step 0 62 0x3f7cf1c305f3c70c false
        (natWord_step 0 61 0x1fbe78e182f9e386 false
        (natWord_step 0 60 0xfdf3c70c17cf1c3 false
        (natWord_step 0 59 0x7ef9e3860be78e1 true
        (natWord_step 0 58 0x3f7cf1c305f3c70 true
        (natWord_step 0 57 0x1fbe78e182f9e38 false
        (natWord_step 0 56 0xfdf3c70c17cf1c false
        (natWord_step 0 55 0x7ef9e3860be78e false
        (natWord_step 0 54 0x3f7cf1c305f3c7 false
        (natWord_step 0 53 0x1fbe78e182f9e3 true
        (natWord_step 0 52 0xfdf3c70c17cf1 true
        (natWord_step 0 51 0x7ef9e3860be78 true
        (natWord_step 0 50 0x3f7cf1c305f3c false
        (natWord_step 0 49 0x1fbe78e182f9e false
        (natWord_step 0 48 0xfdf3c70c17cf false
        (natWord_step 0 47 0x7ef9e3860be7 true
        (natWord_step 0 46 0x3f7cf1c305f3 true
        (natWord_step 0 45 0x1fbe78e182f9 true
        (natWord_step 0 44 0xfdf3c70c17c true
        (natWord_step 0 43 0x7ef9e3860be false
        (natWord_step 0 42 0x3f7cf1c305f false
        (natWord_step 0 41 0x1fbe78e182f true
        (natWord_step 0 40 0xfdf3c70c17 true
        (natWord_step 0 39 0x7ef9e3860b true
        (natWord_step 0 38 0x3f7cf1c305 true
        (natWord_step 0 37 0x1fbe78e182 true
        (natWord_step 0 36 0xfdf3c70c1 false
        (natWord_step 0 35 0x7ef9e3860 true
        (natWord_step 0 34 0x3f7cf1c30 false
        (natWord_step 0 33 0x1fbe78e18 false
        (natWord_step 0 32 0xfdf3c70c false
        (natWord_step 0 31 0x7ef9e386 false
        (natWord_step 0 30 0x3f7cf1c3 false
        (natWord_step 0 29 0x1fbe78e1 true
        (natWord_step 0 28 0xfdf3c70 true
        (natWord_step 0 27 0x7ef9e38 false
        (natWord_step 0 26 0x3f7cf1c false
        (natWord_step 0 25 0x1fbe78e false
        (natWord_step 0 24 0xfdf3c7 false
        (natWord_step 0 23 0x7ef9e3 true
        (natWord_step 0 22 0x3f7cf1 true
        (natWord_step 0 21 0x1fbe78 true
        (natWord_step 0 20 0xfdf3c false
        (natWord_step 0 19 0x7ef9e false
        (natWord_step 0 18 0x3f7cf false
        (natWord_step 0 17 0x1fbe7 true
        (natWord_step 0 16 0xfdf3 true
        (natWord_step 0 15 0x7ef9 true
        (natWord_step 0 14 0x3f7c true
        (natWord_step 0 13 0x1fbe false
        (natWord_step 0 12 0xfdf false
        (natWord_step 0 11 0x7ef true
        (natWord_step 0 10 0x3f7 true
        (natWord_step 0 9 0x1fb true
        (natWord_step 0 8 0xfd true
        (natWord_step 0 7 0x7e true
        (natWord_step 0 6 0x3f false
        (natWord_step 0 5 0x1f true
        (natWord_step 0 4 0xf true
        (natWord_step 0 3 0x7 true
        (natWord_step 0 2 0x3 true
        (natWord_step 0 1 0x1 true
        (natWord_step 0 0 0x0 true
        (Eq.refl (0 : Nat))
        (by decide : natBit 0 = true))
        (by decide : natBit 1 = true))
        (by decide : natBit 2 = true))
        (by decide : natBit 3 = true))
        (by decide : natBit 4 = true))
        (by decide : natBit 5 = true))
        (by decide : natBit 6 = false))
        (by decide : natBit 7 = true))
        (by decide : natBit 8 = true))
        (by decide : natBit 9 = true))
        (by decide : natBit 10 = true))
        (by decide : natBit 11 = true))
        (by decide : natBit 12 = false))
        (by decide : natBit 13 = false))
        (by decide : natBit 14 = true))
        (by decide : natBit 15 = true))
        (by decide : natBit 16 = true))
        (by decide : natBit 17 = true))
        (by decide : natBit 18 = false))
        (by decide : natBit 19 = false))
        (by decide : natBit 20 = false))
        (by decide : natBit 21 = true))
        (by decide : natBit 22 = true))
        (by decide : natBit 23 = true))
        (by decide : natBit 24 = false))
        (by decide : natBit 25 = false))
        (by decide : natBit 26 = false))
        (by decide : natBit 27 = false))
        (by decide : natBit 28 = true))
        (by decide : natBit 29 = true))
        (by decide : natBit 30 = false))
        (by decide : natBit 31 = false))
        (by decide : natBit 32 = false))
        (by decide : natBit 33 = false))
        (by decide : natBit 34 = false))
        (by decide : natBit 35 = true))
        (by decide : natBit 36 = false))
        (by decide : natBit 37 = true))
        (by decide : natBit 38 = true))
        (by decide : natBit 39 = true))
        (by decide : natBit 40 = true))
        (by decide : natBit 41 = true))
        (by decide : natBit 42 = false))
        (by decide : natBit 43 = false))
        (by decide : natBit 44 = true))
        (by decide : natBit 45 = true))
        (by decide : natBit 46 = true))
        (by decide : natBit 47 = true))
        (by decide : natBit 48 = false))
        (by decide : natBit 49 = false))
        (by decide : natBit 50 = false))
        (by decide : natBit 51 = true))
        (by decide : natBit 52 = true))
        (by decide : natBit 53 = true))
        (by decide : natBit 54 = false))
        (by decide : natBit 55 = false))
        (by decide : natBit 56 = false))
        (by decide : natBit 57 = false))
        (by decide : natBit 58 = true))
        (by decide : natBit 59 = true))
        (by decide : natBit 60 = false))
        (by decide : natBit 61 = false))
        (by decide : natBit 62 = false))
        (by decide : natBit 63 = false))

    theorem nat_word_0 : wordWith room_as_nat 0 64 = 0xfdf3c70c17cf1c30 :=
      Eq.trans (word_reads_nat 0 64) nat_word_0_calculated

    theorem nat_word_1_calculated : natWord 1 64 = 0x403c70c30401c30c :=
      (natWord_step 1 63 0x201e38618200e186 false
        (natWord_step 1 62 0x100f1c30c10070c3 false
        (natWord_step 1 61 0x8078e1860803861 true
        (natWord_step 1 60 0x403c70c30401c30 true
        (natWord_step 1 59 0x201e38618200e18 false
        (natWord_step 1 58 0x100f1c30c10070c false
        (natWord_step 1 57 0x8078e186080386 false
        (natWord_step 1 56 0x403c70c30401c3 false
        (natWord_step 1 55 0x201e38618200e1 true
        (natWord_step 1 54 0x100f1c30c10070 true
        (natWord_step 1 53 0x8078e18608038 false
        (natWord_step 1 52 0x403c70c30401c false
        (natWord_step 1 51 0x201e38618200e false
        (natWord_step 1 50 0x100f1c30c1007 false
        (natWord_step 1 49 0x8078e1860803 true
        (natWord_step 1 48 0x403c70c30401 true
        (natWord_step 1 47 0x201e38618200 true
        (natWord_step 1 46 0x100f1c30c100 false
        (natWord_step 1 45 0x8078e186080 false
        (natWord_step 1 44 0x403c70c3040 false
        (natWord_step 1 43 0x201e3861820 false
        (natWord_step 1 42 0x100f1c30c10 false
        (natWord_step 1 41 0x8078e18608 false
        (natWord_step 1 40 0x403c70c304 false
        (natWord_step 1 39 0x201e386182 false
        (natWord_step 1 38 0x100f1c30c1 false
        (natWord_step 1 37 0x8078e1860 true
        (natWord_step 1 36 0x403c70c30 false
        (natWord_step 1 35 0x201e38618 false
        (natWord_step 1 34 0x100f1c30c false
        (natWord_step 1 33 0x8078e186 false
        (natWord_step 1 32 0x403c70c3 false
        (natWord_step 1 31 0x201e3861 true
        (natWord_step 1 30 0x100f1c30 true
        (natWord_step 1 29 0x8078e18 false
        (natWord_step 1 28 0x403c70c false
        (natWord_step 1 27 0x201e386 false
        (natWord_step 1 26 0x100f1c3 false
        (natWord_step 1 25 0x8078e1 true
        (natWord_step 1 24 0x403c70 true
        (natWord_step 1 23 0x201e38 false
        (natWord_step 1 22 0x100f1c false
        (natWord_step 1 21 0x8078e false
        (natWord_step 1 20 0x403c7 false
        (natWord_step 1 19 0x201e3 true
        (natWord_step 1 18 0x100f1 true
        (natWord_step 1 17 0x8078 true
        (natWord_step 1 16 0x403c false
        (natWord_step 1 15 0x201e false
        (natWord_step 1 14 0x100f false
        (natWord_step 1 13 0x807 true
        (natWord_step 1 12 0x403 true
        (natWord_step 1 11 0x201 true
        (natWord_step 1 10 0x100 true
        (natWord_step 1 9 0x80 false
        (natWord_step 1 8 0x40 false
        (natWord_step 1 7 0x20 false
        (natWord_step 1 6 0x10 false
        (natWord_step 1 5 0x8 false
        (natWord_step 1 4 0x4 false
        (natWord_step 1 3 0x2 false
        (natWord_step 1 2 0x1 false
        (natWord_step 1 1 0x0 true
        (natWord_step 1 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 64 = false))
        (by decide : natBit 65 = true))
        (by decide : natBit 66 = false))
        (by decide : natBit 67 = false))
        (by decide : natBit 68 = false))
        (by decide : natBit 69 = false))
        (by decide : natBit 70 = false))
        (by decide : natBit 71 = false))
        (by decide : natBit 72 = false))
        (by decide : natBit 73 = false))
        (by decide : natBit 74 = true))
        (by decide : natBit 75 = true))
        (by decide : natBit 76 = true))
        (by decide : natBit 77 = true))
        (by decide : natBit 78 = false))
        (by decide : natBit 79 = false))
        (by decide : natBit 80 = false))
        (by decide : natBit 81 = true))
        (by decide : natBit 82 = true))
        (by decide : natBit 83 = true))
        (by decide : natBit 84 = false))
        (by decide : natBit 85 = false))
        (by decide : natBit 86 = false))
        (by decide : natBit 87 = false))
        (by decide : natBit 88 = true))
        (by decide : natBit 89 = true))
        (by decide : natBit 90 = false))
        (by decide : natBit 91 = false))
        (by decide : natBit 92 = false))
        (by decide : natBit 93 = false))
        (by decide : natBit 94 = true))
        (by decide : natBit 95 = true))
        (by decide : natBit 96 = false))
        (by decide : natBit 97 = false))
        (by decide : natBit 98 = false))
        (by decide : natBit 99 = false))
        (by decide : natBit 100 = false))
        (by decide : natBit 101 = true))
        (by decide : natBit 102 = false))
        (by decide : natBit 103 = false))
        (by decide : natBit 104 = false))
        (by decide : natBit 105 = false))
        (by decide : natBit 106 = false))
        (by decide : natBit 107 = false))
        (by decide : natBit 108 = false))
        (by decide : natBit 109 = false))
        (by decide : natBit 110 = false))
        (by decide : natBit 111 = true))
        (by decide : natBit 112 = true))
        (by decide : natBit 113 = true))
        (by decide : natBit 114 = false))
        (by decide : natBit 115 = false))
        (by decide : natBit 116 = false))
        (by decide : natBit 117 = false))
        (by decide : natBit 118 = true))
        (by decide : natBit 119 = true))
        (by decide : natBit 120 = false))
        (by decide : natBit 121 = false))
        (by decide : natBit 122 = false))
        (by decide : natBit 123 = false))
        (by decide : natBit 124 = true))
        (by decide : natBit 125 = true))
        (by decide : natBit 126 = false))
        (by decide : natBit 127 = false))

    theorem nat_word_1 : wordWith room_as_nat 1 64 = 0x403c70c30401c30c :=
      Eq.trans (word_reads_nat 1 64) nat_word_1_calculated

    theorem nat_word_2_calculated : natWord 2 64 = 0x30400c1041000040 :=
      (natWord_step 2 63 0x1820060820800020 false
        (natWord_step 2 62 0xc10030410400010 false
        (natWord_step 2 61 0x608018208200008 false
        (natWord_step 2 60 0x30400c104100004 false
        (natWord_step 2 59 0x182006082080002 false
        (natWord_step 2 58 0xc1003041040001 false
        (natWord_step 2 57 0x60801820820000 true
        (natWord_step 2 56 0x30400c10410000 false
        (natWord_step 2 55 0x18200608208000 false
        (natWord_step 2 54 0xc100304104000 false
        (natWord_step 2 53 0x6080182082000 false
        (natWord_step 2 52 0x30400c1041000 false
        (natWord_step 2 51 0x1820060820800 false
        (natWord_step 2 50 0xc10030410400 false
        (natWord_step 2 49 0x608018208200 false
        (natWord_step 2 48 0x30400c104100 false
        (natWord_step 2 47 0x182006082080 false
        (natWord_step 2 46 0xc1003041040 false
        (natWord_step 2 45 0x60801820820 false
        (natWord_step 2 44 0x30400c10410 false
        (natWord_step 2 43 0x18200608208 false
        (natWord_step 2 42 0xc100304104 false
        (natWord_step 2 41 0x6080182082 false
        (natWord_step 2 40 0x30400c1041 false
        (natWord_step 2 39 0x1820060820 true
        (natWord_step 2 38 0xc10030410 false
        (natWord_step 2 37 0x608018208 false
        (natWord_step 2 36 0x30400c104 false
        (natWord_step 2 35 0x182006082 false
        (natWord_step 2 34 0xc1003041 false
        (natWord_step 2 33 0x60801820 true
        (natWord_step 2 32 0x30400c10 false
        (natWord_step 2 31 0x18200608 false
        (natWord_step 2 30 0xc100304 false
        (natWord_step 2 29 0x6080182 false
        (natWord_step 2 28 0x30400c1 false
        (natWord_step 2 27 0x1820060 true
        (natWord_step 2 26 0xc10030 false
        (natWord_step 2 25 0x608018 false
        (natWord_step 2 24 0x30400c false
        (natWord_step 2 23 0x182006 false
        (natWord_step 2 22 0xc1003 false
        (natWord_step 2 21 0x60801 true
        (natWord_step 2 20 0x30400 true
        (natWord_step 2 19 0x18200 false
        (natWord_step 2 18 0xc100 false
        (natWord_step 2 17 0x6080 false
        (natWord_step 2 16 0x3040 false
        (natWord_step 2 15 0x1820 false
        (natWord_step 2 14 0xc10 false
        (natWord_step 2 13 0x608 false
        (natWord_step 2 12 0x304 false
        (natWord_step 2 11 0x182 false
        (natWord_step 2 10 0xc1 false
        (natWord_step 2 9 0x60 true
        (natWord_step 2 8 0x30 false
        (natWord_step 2 7 0x18 false
        (natWord_step 2 6 0xc false
        (natWord_step 2 5 0x6 false
        (natWord_step 2 4 0x3 false
        (natWord_step 2 3 0x1 true
        (natWord_step 2 2 0x0 true
        (natWord_step 2 1 0x0 false
        (natWord_step 2 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 128 = false))
        (by decide : natBit 129 = false))
        (by decide : natBit 130 = true))
        (by decide : natBit 131 = true))
        (by decide : natBit 132 = false))
        (by decide : natBit 133 = false))
        (by decide : natBit 134 = false))
        (by decide : natBit 135 = false))
        (by decide : natBit 136 = false))
        (by decide : natBit 137 = true))
        (by decide : natBit 138 = false))
        (by decide : natBit 139 = false))
        (by decide : natBit 140 = false))
        (by decide : natBit 141 = false))
        (by decide : natBit 142 = false))
        (by decide : natBit 143 = false))
        (by decide : natBit 144 = false))
        (by decide : natBit 145 = false))
        (by decide : natBit 146 = false))
        (by decide : natBit 147 = false))
        (by decide : natBit 148 = true))
        (by decide : natBit 149 = true))
        (by decide : natBit 150 = false))
        (by decide : natBit 151 = false))
        (by decide : natBit 152 = false))
        (by decide : natBit 153 = false))
        (by decide : natBit 154 = false))
        (by decide : natBit 155 = true))
        (by decide : natBit 156 = false))
        (by decide : natBit 157 = false))
        (by decide : natBit 158 = false))
        (by decide : natBit 159 = false))
        (by decide : natBit 160 = false))
        (by decide : natBit 161 = true))
        (by decide : natBit 162 = false))
        (by decide : natBit 163 = false))
        (by decide : natBit 164 = false))
        (by decide : natBit 165 = false))
        (by decide : natBit 166 = false))
        (by decide : natBit 167 = true))
        (by decide : natBit 168 = false))
        (by decide : natBit 169 = false))
        (by decide : natBit 170 = false))
        (by decide : natBit 171 = false))
        (by decide : natBit 172 = false))
        (by decide : natBit 173 = false))
        (by decide : natBit 174 = false))
        (by decide : natBit 175 = false))
        (by decide : natBit 176 = false))
        (by decide : natBit 177 = false))
        (by decide : natBit 178 = false))
        (by decide : natBit 179 = false))
        (by decide : natBit 180 = false))
        (by decide : natBit 181 = false))
        (by decide : natBit 182 = false))
        (by decide : natBit 183 = false))
        (by decide : natBit 184 = false))
        (by decide : natBit 185 = true))
        (by decide : natBit 186 = false))
        (by decide : natBit 187 = false))
        (by decide : natBit 188 = false))
        (by decide : natBit 189 = false))
        (by decide : natBit 190 = false))
        (by decide : natBit 191 = false))

    theorem nat_word_2 : wordWith room_as_nat 2 64 = 0x30400c1041000040 :=
      Eq.trans (word_reads_nat 2 64) nat_word_2_calculated

    theorem nat_word_3_calculated : natWord 3 64 = 0x7cf1c30403 :=
      (natWord_step 3 63 0x3e78e18201 true
        (natWord_step 3 62 0x1f3c70c100 true
        (natWord_step 3 61 0xf9e386080 false
        (natWord_step 3 60 0x7cf1c3040 false
        (natWord_step 3 59 0x3e78e1820 false
        (natWord_step 3 58 0x1f3c70c10 false
        (natWord_step 3 57 0xf9e38608 false
        (natWord_step 3 56 0x7cf1c304 false
        (natWord_step 3 55 0x3e78e182 false
        (natWord_step 3 54 0x1f3c70c1 false
        (natWord_step 3 53 0xf9e3860 true
        (natWord_step 3 52 0x7cf1c30 false
        (natWord_step 3 51 0x3e78e18 false
        (natWord_step 3 50 0x1f3c70c false
        (natWord_step 3 49 0xf9e386 false
        (natWord_step 3 48 0x7cf1c3 false
        (natWord_step 3 47 0x3e78e1 true
        (natWord_step 3 46 0x1f3c70 true
        (natWord_step 3 45 0xf9e38 false
        (natWord_step 3 44 0x7cf1c false
        (natWord_step 3 43 0x3e78e false
        (natWord_step 3 42 0x1f3c7 false
        (natWord_step 3 41 0xf9e3 true
        (natWord_step 3 40 0x7cf1 true
        (natWord_step 3 39 0x3e78 true
        (natWord_step 3 38 0x1f3c false
        (natWord_step 3 37 0xf9e false
        (natWord_step 3 36 0x7cf false
        (natWord_step 3 35 0x3e7 true
        (natWord_step 3 34 0x1f3 true
        (natWord_step 3 33 0xf9 true
        (natWord_step 3 32 0x7c true
        (natWord_step 3 31 0x3e false
        (natWord_step 3 30 0x1f false
        (natWord_step 3 29 0xf true
        (natWord_step 3 28 0x7 true
        (natWord_step 3 27 0x3 true
        (natWord_step 3 26 0x1 true
        (natWord_step 3 25 0x0 true
        (natWord_step 3 24 0x0 false
        (natWord_step 3 23 0x0 false
        (natWord_step 3 22 0x0 false
        (natWord_step 3 21 0x0 false
        (natWord_step 3 20 0x0 false
        (natWord_step 3 19 0x0 false
        (natWord_step 3 18 0x0 false
        (natWord_step 3 17 0x0 false
        (natWord_step 3 16 0x0 false
        (natWord_step 3 15 0x0 false
        (natWord_step 3 14 0x0 false
        (natWord_step 3 13 0x0 false
        (natWord_step 3 12 0x0 false
        (natWord_step 3 11 0x0 false
        (natWord_step 3 10 0x0 false
        (natWord_step 3 9 0x0 false
        (natWord_step 3 8 0x0 false
        (natWord_step 3 7 0x0 false
        (natWord_step 3 6 0x0 false
        (natWord_step 3 5 0x0 false
        (natWord_step 3 4 0x0 false
        (natWord_step 3 3 0x0 false
        (natWord_step 3 2 0x0 false
        (natWord_step 3 1 0x0 false
        (natWord_step 3 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 192 = false))
        (by decide : natBit 193 = false))
        (by decide : natBit 194 = false))
        (by decide : natBit 195 = false))
        (by decide : natBit 196 = false))
        (by decide : natBit 197 = false))
        (by decide : natBit 198 = false))
        (by decide : natBit 199 = false))
        (by decide : natBit 200 = false))
        (by decide : natBit 201 = false))
        (by decide : natBit 202 = false))
        (by decide : natBit 203 = false))
        (by decide : natBit 204 = false))
        (by decide : natBit 205 = false))
        (by decide : natBit 206 = false))
        (by decide : natBit 207 = false))
        (by decide : natBit 208 = false))
        (by decide : natBit 209 = false))
        (by decide : natBit 210 = false))
        (by decide : natBit 211 = false))
        (by decide : natBit 212 = false))
        (by decide : natBit 213 = false))
        (by decide : natBit 214 = false))
        (by decide : natBit 215 = false))
        (by decide : natBit 216 = false))
        (by decide : natBit 217 = true))
        (by decide : natBit 218 = true))
        (by decide : natBit 219 = true))
        (by decide : natBit 220 = true))
        (by decide : natBit 221 = true))
        (by decide : natBit 222 = false))
        (by decide : natBit 223 = false))
        (by decide : natBit 224 = true))
        (by decide : natBit 225 = true))
        (by decide : natBit 226 = true))
        (by decide : natBit 227 = true))
        (by decide : natBit 228 = false))
        (by decide : natBit 229 = false))
        (by decide : natBit 230 = false))
        (by decide : natBit 231 = true))
        (by decide : natBit 232 = true))
        (by decide : natBit 233 = true))
        (by decide : natBit 234 = false))
        (by decide : natBit 235 = false))
        (by decide : natBit 236 = false))
        (by decide : natBit 237 = false))
        (by decide : natBit 238 = true))
        (by decide : natBit 239 = true))
        (by decide : natBit 240 = false))
        (by decide : natBit 241 = false))
        (by decide : natBit 242 = false))
        (by decide : natBit 243 = false))
        (by decide : natBit 244 = false))
        (by decide : natBit 245 = true))
        (by decide : natBit 246 = false))
        (by decide : natBit 247 = false))
        (by decide : natBit 248 = false))
        (by decide : natBit 249 = false))
        (by decide : natBit 250 = false))
        (by decide : natBit 251 = false))
        (by decide : natBit 252 = false))
        (by decide : natBit 253 = false))
        (by decide : natBit 254 = true))
        (by decide : natBit 255 = true))

    theorem nat_word_3 : wordWith room_as_nat 3 64 = 0x7cf1c30403 :=
      Eq.trans (word_reads_nat 3 64) nat_word_3_calculated

    theorem nat_word_4_calculated : natWord 4 64 = 0xc70c30401c30c304 :=
      (natWord_step 4 63 0x638618200e186182 false
        (natWord_step 4 62 0x31c30c10070c30c1 false
        (natWord_step 4 61 0x18e1860803861860 true
        (natWord_step 4 60 0xc70c30401c30c30 false
        (natWord_step 4 59 0x638618200e18618 false
        (natWord_step 4 58 0x31c30c10070c30c false
        (natWord_step 4 57 0x18e186080386186 false
        (natWord_step 4 56 0xc70c30401c30c3 false
        (natWord_step 4 55 0x638618200e1861 true
        (natWord_step 4 54 0x31c30c10070c30 true
        (natWord_step 4 53 0x18e18608038618 false
        (natWord_step 4 52 0xc70c30401c30c false
        (natWord_step 4 51 0x638618200e186 false
        (natWord_step 4 50 0x31c30c10070c3 false
        (natWord_step 4 49 0x18e1860803861 true
        (natWord_step 4 48 0xc70c30401c30 true
        (natWord_step 4 47 0x638618200e18 false
        (natWord_step 4 46 0x31c30c10070c false
        (natWord_step 4 45 0x18e186080386 false
        (natWord_step 4 44 0xc70c30401c3 false
        (natWord_step 4 43 0x638618200e1 true
        (natWord_step 4 42 0x31c30c10070 true
        (natWord_step 4 41 0x18e18608038 false
        (natWord_step 4 40 0xc70c30401c false
        (natWord_step 4 39 0x638618200e false
        (natWord_step 4 38 0x31c30c1007 false
        (natWord_step 4 37 0x18e1860803 true
        (natWord_step 4 36 0xc70c30401 true
        (natWord_step 4 35 0x638618200 true
        (natWord_step 4 34 0x31c30c100 false
        (natWord_step 4 33 0x18e186080 false
        (natWord_step 4 32 0xc70c3040 false
        (natWord_step 4 31 0x63861820 false
        (natWord_step 4 30 0x31c30c10 false
        (natWord_step 4 29 0x18e18608 false
        (natWord_step 4 28 0xc70c304 false
        (natWord_step 4 27 0x6386182 false
        (natWord_step 4 26 0x31c30c1 false
        (natWord_step 4 25 0x18e1860 true
        (natWord_step 4 24 0xc70c30 false
        (natWord_step 4 23 0x638618 false
        (natWord_step 4 22 0x31c30c false
        (natWord_step 4 21 0x18e186 false
        (natWord_step 4 20 0xc70c3 false
        (natWord_step 4 19 0x63861 true
        (natWord_step 4 18 0x31c30 true
        (natWord_step 4 17 0x18e18 false
        (natWord_step 4 16 0xc70c false
        (natWord_step 4 15 0x6386 false
        (natWord_step 4 14 0x31c3 false
        (natWord_step 4 13 0x18e1 true
        (natWord_step 4 12 0xc70 true
        (natWord_step 4 11 0x638 false
        (natWord_step 4 10 0x31c false
        (natWord_step 4 9 0x18e false
        (natWord_step 4 8 0xc7 false
        (natWord_step 4 7 0x63 true
        (natWord_step 4 6 0x31 true
        (natWord_step 4 5 0x18 true
        (natWord_step 4 4 0xc false
        (natWord_step 4 3 0x6 false
        (natWord_step 4 2 0x3 false
        (natWord_step 4 1 0x1 true
        (natWord_step 4 0 0x0 true
        (Eq.refl (0 : Nat))
        (by decide : natBit 256 = true))
        (by decide : natBit 257 = true))
        (by decide : natBit 258 = false))
        (by decide : natBit 259 = false))
        (by decide : natBit 260 = false))
        (by decide : natBit 261 = true))
        (by decide : natBit 262 = true))
        (by decide : natBit 263 = true))
        (by decide : natBit 264 = false))
        (by decide : natBit 265 = false))
        (by decide : natBit 266 = false))
        (by decide : natBit 267 = false))
        (by decide : natBit 268 = true))
        (by decide : natBit 269 = true))
        (by decide : natBit 270 = false))
        (by decide : natBit 271 = false))
        (by decide : natBit 272 = false))
        (by decide : natBit 273 = false))
        (by decide : natBit 274 = true))
        (by decide : natBit 275 = true))
        (by decide : natBit 276 = false))
        (by decide : natBit 277 = false))
        (by decide : natBit 278 = false))
        (by decide : natBit 279 = false))
        (by decide : natBit 280 = false))
        (by decide : natBit 281 = true))
        (by decide : natBit 282 = false))
        (by decide : natBit 283 = false))
        (by decide : natBit 284 = false))
        (by decide : natBit 285 = false))
        (by decide : natBit 286 = false))
        (by decide : natBit 287 = false))
        (by decide : natBit 288 = false))
        (by decide : natBit 289 = false))
        (by decide : natBit 290 = false))
        (by decide : natBit 291 = true))
        (by decide : natBit 292 = true))
        (by decide : natBit 293 = true))
        (by decide : natBit 294 = false))
        (by decide : natBit 295 = false))
        (by decide : natBit 296 = false))
        (by decide : natBit 297 = false))
        (by decide : natBit 298 = true))
        (by decide : natBit 299 = true))
        (by decide : natBit 300 = false))
        (by decide : natBit 301 = false))
        (by decide : natBit 302 = false))
        (by decide : natBit 303 = false))
        (by decide : natBit 304 = true))
        (by decide : natBit 305 = true))
        (by decide : natBit 306 = false))
        (by decide : natBit 307 = false))
        (by decide : natBit 308 = false))
        (by decide : natBit 309 = false))
        (by decide : natBit 310 = true))
        (by decide : natBit 311 = true))
        (by decide : natBit 312 = false))
        (by decide : natBit 313 = false))
        (by decide : natBit 314 = false))
        (by decide : natBit 315 = false))
        (by decide : natBit 316 = false))
        (by decide : natBit 317 = true))
        (by decide : natBit 318 = false))
        (by decide : natBit 319 = false))

    theorem nat_word_4 : wordWith room_as_nat 4 64 = 0xc70c30401c30c304 :=
      Eq.trans (word_reads_nat 4 64) nat_word_4_calculated

    theorem nat_word_5_calculated : natWord 5 64 = 0xc30c3040041041 :=
      (natWord_step 5 63 0x61861820020820 true
        (natWord_step 5 62 0x30c30c10010410 false
        (natWord_step 5 61 0x18618608008208 false
        (natWord_step 5 60 0xc30c304004104 false
        (natWord_step 5 59 0x6186182002082 false
        (natWord_step 5 58 0x30c30c1001041 false
        (natWord_step 5 57 0x1861860800820 true
        (natWord_step 5 56 0xc30c30400410 false
        (natWord_step 5 55 0x618618200208 false
        (natWord_step 5 54 0x30c30c100104 false
        (natWord_step 5 53 0x186186080082 false
        (natWord_step 5 52 0xc30c3040041 false
        (natWord_step 5 51 0x61861820020 true
        (natWord_step 5 50 0x30c30c10010 false
        (natWord_step 5 49 0x18618608008 false
        (natWord_step 5 48 0xc30c304004 false
        (natWord_step 5 47 0x6186182002 false
        (natWord_step 5 46 0x30c30c1001 false
        (natWord_step 5 45 0x1861860800 true
        (natWord_step 5 44 0xc30c30400 false
        (natWord_step 5 43 0x618618200 false
        (natWord_step 5 42 0x30c30c100 false
        (natWord_step 5 41 0x186186080 false
        (natWord_step 5 40 0xc30c3040 false
        (natWord_step 5 39 0x61861820 false
        (natWord_step 5 38 0x30c30c10 false
        (natWord_step 5 37 0x18618608 false
        (natWord_step 5 36 0xc30c304 false
        (natWord_step 5 35 0x6186182 false
        (natWord_step 5 34 0x30c30c1 false
        (natWord_step 5 33 0x1861860 true
        (natWord_step 5 32 0xc30c30 false
        (natWord_step 5 31 0x618618 false
        (natWord_step 5 30 0x30c30c false
        (natWord_step 5 29 0x186186 false
        (natWord_step 5 28 0xc30c3 false
        (natWord_step 5 27 0x61861 true
        (natWord_step 5 26 0x30c30 true
        (natWord_step 5 25 0x18618 false
        (natWord_step 5 24 0xc30c false
        (natWord_step 5 23 0x6186 false
        (natWord_step 5 22 0x30c3 false
        (natWord_step 5 21 0x1861 true
        (natWord_step 5 20 0xc30 true
        (natWord_step 5 19 0x618 false
        (natWord_step 5 18 0x30c false
        (natWord_step 5 17 0x186 false
        (natWord_step 5 16 0xc3 false
        (natWord_step 5 15 0x61 true
        (natWord_step 5 14 0x30 true
        (natWord_step 5 13 0x18 false
        (natWord_step 5 12 0xc false
        (natWord_step 5 11 0x6 false
        (natWord_step 5 10 0x3 false
        (natWord_step 5 9 0x1 true
        (natWord_step 5 8 0x0 true
        (natWord_step 5 7 0x0 false
        (natWord_step 5 6 0x0 false
        (natWord_step 5 5 0x0 false
        (natWord_step 5 4 0x0 false
        (natWord_step 5 3 0x0 false
        (natWord_step 5 2 0x0 false
        (natWord_step 5 1 0x0 false
        (natWord_step 5 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 320 = false))
        (by decide : natBit 321 = false))
        (by decide : natBit 322 = false))
        (by decide : natBit 323 = false))
        (by decide : natBit 324 = false))
        (by decide : natBit 325 = false))
        (by decide : natBit 326 = false))
        (by decide : natBit 327 = false))
        (by decide : natBit 328 = true))
        (by decide : natBit 329 = true))
        (by decide : natBit 330 = false))
        (by decide : natBit 331 = false))
        (by decide : natBit 332 = false))
        (by decide : natBit 333 = false))
        (by decide : natBit 334 = true))
        (by decide : natBit 335 = true))
        (by decide : natBit 336 = false))
        (by decide : natBit 337 = false))
        (by decide : natBit 338 = false))
        (by decide : natBit 339 = false))
        (by decide : natBit 340 = true))
        (by decide : natBit 341 = true))
        (by decide : natBit 342 = false))
        (by decide : natBit 343 = false))
        (by decide : natBit 344 = false))
        (by decide : natBit 345 = false))
        (by decide : natBit 346 = true))
        (by decide : natBit 347 = true))
        (by decide : natBit 348 = false))
        (by decide : natBit 349 = false))
        (by decide : natBit 350 = false))
        (by decide : natBit 351 = false))
        (by decide : natBit 352 = false))
        (by decide : natBit 353 = true))
        (by decide : natBit 354 = false))
        (by decide : natBit 355 = false))
        (by decide : natBit 356 = false))
        (by decide : natBit 357 = false))
        (by decide : natBit 358 = false))
        (by decide : natBit 359 = false))
        (by decide : natBit 360 = false))
        (by decide : natBit 361 = false))
        (by decide : natBit 362 = false))
        (by decide : natBit 363 = false))
        (by decide : natBit 364 = false))
        (by decide : natBit 365 = true))
        (by decide : natBit 366 = false))
        (by decide : natBit 367 = false))
        (by decide : natBit 368 = false))
        (by decide : natBit 369 = false))
        (by decide : natBit 370 = false))
        (by decide : natBit 371 = true))
        (by decide : natBit 372 = false))
        (by decide : natBit 373 = false))
        (by decide : natBit 374 = false))
        (by decide : natBit 375 = false))
        (by decide : natBit 376 = false))
        (by decide : natBit 377 = true))
        (by decide : natBit 378 = false))
        (by decide : natBit 379 = false))
        (by decide : natBit 380 = false))
        (by decide : natBit 381 = false))
        (by decide : natBit 382 = false))
        (by decide : natBit 383 = true))

    theorem nat_word_5 : wordWith room_as_nat 5 64 = 0xc30c3040041041 :=
      Eq.trans (word_reads_nat 5 64) nat_word_5_calculated

    theorem nat_word_6_calculated : natWord 6 64 = 0x3c70 :=
      (natWord_step 6 63 0x1e38 false
        (natWord_step 6 62 0xf1c false
        (natWord_step 6 61 0x78e false
        (natWord_step 6 60 0x3c7 false
        (natWord_step 6 59 0x1e3 true
        (natWord_step 6 58 0xf1 true
        (natWord_step 6 57 0x78 true
        (natWord_step 6 56 0x3c false
        (natWord_step 6 55 0x1e false
        (natWord_step 6 54 0xf false
        (natWord_step 6 53 0x7 true
        (natWord_step 6 52 0x3 true
        (natWord_step 6 51 0x1 true
        (natWord_step 6 50 0x0 true
        (natWord_step 6 49 0x0 false
        (natWord_step 6 48 0x0 false
        (natWord_step 6 47 0x0 false
        (natWord_step 6 46 0x0 false
        (natWord_step 6 45 0x0 false
        (natWord_step 6 44 0x0 false
        (natWord_step 6 43 0x0 false
        (natWord_step 6 42 0x0 false
        (natWord_step 6 41 0x0 false
        (natWord_step 6 40 0x0 false
        (natWord_step 6 39 0x0 false
        (natWord_step 6 38 0x0 false
        (natWord_step 6 37 0x0 false
        (natWord_step 6 36 0x0 false
        (natWord_step 6 35 0x0 false
        (natWord_step 6 34 0x0 false
        (natWord_step 6 33 0x0 false
        (natWord_step 6 32 0x0 false
        (natWord_step 6 31 0x0 false
        (natWord_step 6 30 0x0 false
        (natWord_step 6 29 0x0 false
        (natWord_step 6 28 0x0 false
        (natWord_step 6 27 0x0 false
        (natWord_step 6 26 0x0 false
        (natWord_step 6 25 0x0 false
        (natWord_step 6 24 0x0 false
        (natWord_step 6 23 0x0 false
        (natWord_step 6 22 0x0 false
        (natWord_step 6 21 0x0 false
        (natWord_step 6 20 0x0 false
        (natWord_step 6 19 0x0 false
        (natWord_step 6 18 0x0 false
        (natWord_step 6 17 0x0 false
        (natWord_step 6 16 0x0 false
        (natWord_step 6 15 0x0 false
        (natWord_step 6 14 0x0 false
        (natWord_step 6 13 0x0 false
        (natWord_step 6 12 0x0 false
        (natWord_step 6 11 0x0 false
        (natWord_step 6 10 0x0 false
        (natWord_step 6 9 0x0 false
        (natWord_step 6 8 0x0 false
        (natWord_step 6 7 0x0 false
        (natWord_step 6 6 0x0 false
        (natWord_step 6 5 0x0 false
        (natWord_step 6 4 0x0 false
        (natWord_step 6 3 0x0 false
        (natWord_step 6 2 0x0 false
        (natWord_step 6 1 0x0 false
        (natWord_step 6 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 384 = false))
        (by decide : natBit 385 = false))
        (by decide : natBit 386 = false))
        (by decide : natBit 387 = false))
        (by decide : natBit 388 = false))
        (by decide : natBit 389 = false))
        (by decide : natBit 390 = false))
        (by decide : natBit 391 = false))
        (by decide : natBit 392 = false))
        (by decide : natBit 393 = false))
        (by decide : natBit 394 = false))
        (by decide : natBit 395 = false))
        (by decide : natBit 396 = false))
        (by decide : natBit 397 = false))
        (by decide : natBit 398 = false))
        (by decide : natBit 399 = false))
        (by decide : natBit 400 = false))
        (by decide : natBit 401 = false))
        (by decide : natBit 402 = false))
        (by decide : natBit 403 = false))
        (by decide : natBit 404 = false))
        (by decide : natBit 405 = false))
        (by decide : natBit 406 = false))
        (by decide : natBit 407 = false))
        (by decide : natBit 408 = false))
        (by decide : natBit 409 = false))
        (by decide : natBit 410 = false))
        (by decide : natBit 411 = false))
        (by decide : natBit 412 = false))
        (by decide : natBit 413 = false))
        (by decide : natBit 414 = false))
        (by decide : natBit 415 = false))
        (by decide : natBit 416 = false))
        (by decide : natBit 417 = false))
        (by decide : natBit 418 = false))
        (by decide : natBit 419 = false))
        (by decide : natBit 420 = false))
        (by decide : natBit 421 = false))
        (by decide : natBit 422 = false))
        (by decide : natBit 423 = false))
        (by decide : natBit 424 = false))
        (by decide : natBit 425 = false))
        (by decide : natBit 426 = false))
        (by decide : natBit 427 = false))
        (by decide : natBit 428 = false))
        (by decide : natBit 429 = false))
        (by decide : natBit 430 = false))
        (by decide : natBit 431 = false))
        (by decide : natBit 432 = false))
        (by decide : natBit 433 = false))
        (by decide : natBit 434 = true))
        (by decide : natBit 435 = true))
        (by decide : natBit 436 = true))
        (by decide : natBit 437 = true))
        (by decide : natBit 438 = false))
        (by decide : natBit 439 = false))
        (by decide : natBit 440 = false))
        (by decide : natBit 441 = true))
        (by decide : natBit 442 = true))
        (by decide : natBit 443 = true))
        (by decide : natBit 444 = false))
        (by decide : natBit 445 = false))
        (by decide : natBit 446 = false))
        (by decide : natBit 447 = false))

    theorem nat_word_6 : wordWith room_as_nat 6 64 = 0x3c70 :=
      Eq.trans (word_reads_nat 6 64) nat_word_6_calculated

    theorem nat_word_7_calculated : natWord 7 64 = 0xc30401c30c30400c :=
      (natWord_step 7 63 0x618200e186182006 false
        (natWord_step 7 62 0x30c10070c30c1003 false
        (natWord_step 7 61 0x1860803861860801 true
        (natWord_step 7 60 0xc30401c30c30400 true
        (natWord_step 7 59 0x618200e18618200 false
        (natWord_step 7 58 0x30c10070c30c100 false
        (natWord_step 7 57 0x186080386186080 false
        (natWord_step 7 56 0xc30401c30c3040 false
        (natWord_step 7 55 0x618200e1861820 false
        (natWord_step 7 54 0x30c10070c30c10 false
        (natWord_step 7 53 0x18608038618608 false
        (natWord_step 7 52 0xc30401c30c304 false
        (natWord_step 7 51 0x618200e186182 false
        (natWord_step 7 50 0x30c10070c30c1 false
        (natWord_step 7 49 0x1860803861860 true
        (natWord_step 7 48 0xc30401c30c30 false
        (natWord_step 7 47 0x618200e18618 false
        (natWord_step 7 46 0x30c10070c30c false
        (natWord_step 7 45 0x186080386186 false
        (natWord_step 7 44 0xc30401c30c3 false
        (natWord_step 7 43 0x618200e1861 true
        (natWord_step 7 42 0x30c10070c30 true
        (natWord_step 7 41 0x18608038618 false
        (natWord_step 7 40 0xc30401c30c false
        (natWord_step 7 39 0x618200e186 false
        (natWord_step 7 38 0x30c10070c3 false
        (natWord_step 7 37 0x1860803861 true
        (natWord_step 7 36 0xc30401c30 true
        (natWord_step 7 35 0x618200e18 false
        (natWord_step 7 34 0x30c10070c false
        (natWord_step 7 33 0x186080386 false
        (natWord_step 7 32 0xc30401c3 false
        (natWord_step 7 31 0x618200e1 true
        (natWord_step 7 30 0x30c10070 true
        (natWord_step 7 29 0x18608038 false
        (natWord_step 7 28 0xc30401c false
        (natWord_step 7 27 0x618200e false
        (natWord_step 7 26 0x30c1007 false
        (natWord_step 7 25 0x1860803 true
        (natWord_step 7 24 0xc30401 true
        (natWord_step 7 23 0x618200 true
        (natWord_step 7 22 0x30c100 false
        (natWord_step 7 21 0x186080 false
        (natWord_step 7 20 0xc3040 false
        (natWord_step 7 19 0x61820 false
        (natWord_step 7 18 0x30c10 false
        (natWord_step 7 17 0x18608 false
        (natWord_step 7 16 0xc304 false
        (natWord_step 7 15 0x6182 false
        (natWord_step 7 14 0x30c1 false
        (natWord_step 7 13 0x1860 true
        (natWord_step 7 12 0xc30 false
        (natWord_step 7 11 0x618 false
        (natWord_step 7 10 0x30c false
        (natWord_step 7 9 0x186 false
        (natWord_step 7 8 0xc3 false
        (natWord_step 7 7 0x61 true
        (natWord_step 7 6 0x30 true
        (natWord_step 7 5 0x18 false
        (natWord_step 7 4 0xc false
        (natWord_step 7 3 0x6 false
        (natWord_step 7 2 0x3 false
        (natWord_step 7 1 0x1 true
        (natWord_step 7 0 0x0 true
        (Eq.refl (0 : Nat))
        (by decide : natBit 448 = true))
        (by decide : natBit 449 = true))
        (by decide : natBit 450 = false))
        (by decide : natBit 451 = false))
        (by decide : natBit 452 = false))
        (by decide : natBit 453 = false))
        (by decide : natBit 454 = true))
        (by decide : natBit 455 = true))
        (by decide : natBit 456 = false))
        (by decide : natBit 457 = false))
        (by decide : natBit 458 = false))
        (by decide : natBit 459 = false))
        (by decide : natBit 460 = false))
        (by decide : natBit 461 = true))
        (by decide : natBit 462 = false))
        (by decide : natBit 463 = false))
        (by decide : natBit 464 = false))
        (by decide : natBit 465 = false))
        (by decide : natBit 466 = false))
        (by decide : natBit 467 = false))
        (by decide : natBit 468 = false))
        (by decide : natBit 469 = false))
        (by decide : natBit 470 = false))
        (by decide : natBit 471 = true))
        (by decide : natBit 472 = true))
        (by decide : natBit 473 = true))
        (by decide : natBit 474 = false))
        (by decide : natBit 475 = false))
        (by decide : natBit 476 = false))
        (by decide : natBit 477 = false))
        (by decide : natBit 478 = true))
        (by decide : natBit 479 = true))
        (by decide : natBit 480 = false))
        (by decide : natBit 481 = false))
        (by decide : natBit 482 = false))
        (by decide : natBit 483 = false))
        (by decide : natBit 484 = true))
        (by decide : natBit 485 = true))
        (by decide : natBit 486 = false))
        (by decide : natBit 487 = false))
        (by decide : natBit 488 = false))
        (by decide : natBit 489 = false))
        (by decide : natBit 490 = true))
        (by decide : natBit 491 = true))
        (by decide : natBit 492 = false))
        (by decide : natBit 493 = false))
        (by decide : natBit 494 = false))
        (by decide : natBit 495 = false))
        (by decide : natBit 496 = false))
        (by decide : natBit 497 = true))
        (by decide : natBit 498 = false))
        (by decide : natBit 499 = false))
        (by decide : natBit 500 = false))
        (by decide : natBit 501 = false))
        (by decide : natBit 502 = false))
        (by decide : natBit 503 = false))
        (by decide : natBit 504 = false))
        (by decide : natBit 505 = false))
        (by decide : natBit 506 = false))
        (by decide : natBit 507 = false))
        (by decide : natBit 508 = true))
        (by decide : natBit 509 = true))
        (by decide : natBit 510 = false))
        (by decide : natBit 511 = false))

    theorem nat_word_7 : wordWith room_as_nat 7 64 = 0xc30401c30c30400c :=
      Eq.trans (word_reads_nat 7 64) nat_word_7_calculated

    theorem nat_word_8_calculated : natWord 8 64 = 0x30c30400c30c3040 :=
      (natWord_step 8 63 0x1861820061861820 false
        (natWord_step 8 62 0xc30c10030c30c10 false
        (natWord_step 8 61 0x618608018618608 false
        (natWord_step 8 60 0x30c30400c30c304 false
        (natWord_step 8 59 0x186182006186182 false
        (natWord_step 8 58 0xc30c10030c30c1 false
        (natWord_step 8 57 0x61860801861860 true
        (natWord_step 8 56 0x30c30400c30c30 false
        (natWord_step 8 55 0x18618200618618 false
        (natWord_step 8 54 0xc30c10030c30c false
        (natWord_step 8 53 0x6186080186186 false
        (natWord_step 8 52 0x30c30400c30c3 false
        (natWord_step 8 51 0x1861820061861 true
        (natWord_step 8 50 0xc30c10030c30 true
        (natWord_step 8 49 0x618608018618 false
        (natWord_step 8 48 0x30c30400c30c false
        (natWord_step 8 47 0x186182006186 false
        (natWord_step 8 46 0xc30c10030c3 false
        (natWord_step 8 45 0x61860801861 true
        (natWord_step 8 44 0x30c30400c30 true
        (natWord_step 8 43 0x18618200618 false
        (natWord_step 8 42 0xc30c10030c false
        (natWord_step 8 41 0x6186080186 false
        (natWord_step 8 40 0x30c30400c3 false
        (natWord_step 8 39 0x1861820061 true
        (natWord_step 8 38 0xc30c10030 true
        (natWord_step 8 37 0x618608018 false
        (natWord_step 8 36 0x30c30400c false
        (natWord_step 8 35 0x186182006 false
        (natWord_step 8 34 0xc30c1003 false
        (natWord_step 8 33 0x61860801 true
        (natWord_step 8 32 0x30c30400 true
        (natWord_step 8 31 0x18618200 false
        (natWord_step 8 30 0xc30c100 false
        (natWord_step 8 29 0x6186080 false
        (natWord_step 8 28 0x30c3040 false
        (natWord_step 8 27 0x1861820 false
        (natWord_step 8 26 0xc30c10 false
        (natWord_step 8 25 0x618608 false
        (natWord_step 8 24 0x30c304 false
        (natWord_step 8 23 0x186182 false
        (natWord_step 8 22 0xc30c1 false
        (natWord_step 8 21 0x61860 true
        (natWord_step 8 20 0x30c30 false
        (natWord_step 8 19 0x18618 false
        (natWord_step 8 18 0xc30c false
        (natWord_step 8 17 0x6186 false
        (natWord_step 8 16 0x30c3 false
        (natWord_step 8 15 0x1861 true
        (natWord_step 8 14 0xc30 true
        (natWord_step 8 13 0x618 false
        (natWord_step 8 12 0x30c false
        (natWord_step 8 11 0x186 false
        (natWord_step 8 10 0xc3 false
        (natWord_step 8 9 0x61 true
        (natWord_step 8 8 0x30 true
        (natWord_step 8 7 0x18 false
        (natWord_step 8 6 0xc false
        (natWord_step 8 5 0x6 false
        (natWord_step 8 4 0x3 false
        (natWord_step 8 3 0x1 true
        (natWord_step 8 2 0x0 true
        (natWord_step 8 1 0x0 false
        (natWord_step 8 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 512 = false))
        (by decide : natBit 513 = false))
        (by decide : natBit 514 = true))
        (by decide : natBit 515 = true))
        (by decide : natBit 516 = false))
        (by decide : natBit 517 = false))
        (by decide : natBit 518 = false))
        (by decide : natBit 519 = false))
        (by decide : natBit 520 = true))
        (by decide : natBit 521 = true))
        (by decide : natBit 522 = false))
        (by decide : natBit 523 = false))
        (by decide : natBit 524 = false))
        (by decide : natBit 525 = false))
        (by decide : natBit 526 = true))
        (by decide : natBit 527 = true))
        (by decide : natBit 528 = false))
        (by decide : natBit 529 = false))
        (by decide : natBit 530 = false))
        (by decide : natBit 531 = false))
        (by decide : natBit 532 = false))
        (by decide : natBit 533 = true))
        (by decide : natBit 534 = false))
        (by decide : natBit 535 = false))
        (by decide : natBit 536 = false))
        (by decide : natBit 537 = false))
        (by decide : natBit 538 = false))
        (by decide : natBit 539 = false))
        (by decide : natBit 540 = false))
        (by decide : natBit 541 = false))
        (by decide : natBit 542 = false))
        (by decide : natBit 543 = false))
        (by decide : natBit 544 = true))
        (by decide : natBit 545 = true))
        (by decide : natBit 546 = false))
        (by decide : natBit 547 = false))
        (by decide : natBit 548 = false))
        (by decide : natBit 549 = false))
        (by decide : natBit 550 = true))
        (by decide : natBit 551 = true))
        (by decide : natBit 552 = false))
        (by decide : natBit 553 = false))
        (by decide : natBit 554 = false))
        (by decide : natBit 555 = false))
        (by decide : natBit 556 = true))
        (by decide : natBit 557 = true))
        (by decide : natBit 558 = false))
        (by decide : natBit 559 = false))
        (by decide : natBit 560 = false))
        (by decide : natBit 561 = false))
        (by decide : natBit 562 = true))
        (by decide : natBit 563 = true))
        (by decide : natBit 564 = false))
        (by decide : natBit 565 = false))
        (by decide : natBit 566 = false))
        (by decide : natBit 567 = false))
        (by decide : natBit 568 = false))
        (by decide : natBit 569 = true))
        (by decide : natBit 570 = false))
        (by decide : natBit 571 = false))
        (by decide : natBit 572 = false))
        (by decide : natBit 573 = false))
        (by decide : natBit 574 = false))
        (by decide : natBit 575 = false))

    theorem nat_word_8 : wordWith room_as_nat 8 64 = 0x30c30400c30c3040 :=
      Eq.trans (word_reads_nat 8 64) nat_word_8_calculated

    theorem nat_word_9_calculated : natWord 9 64 = 0x410410000000000 :=
      (natWord_step 9 63 0x208208000000000 false
        (natWord_step 9 62 0x104104000000000 false
        (natWord_step 9 61 0x82082000000000 false
        (natWord_step 9 60 0x41041000000000 false
        (natWord_step 9 59 0x20820800000000 false
        (natWord_step 9 58 0x10410400000000 false
        (natWord_step 9 57 0x8208200000000 false
        (natWord_step 9 56 0x4104100000000 false
        (natWord_step 9 55 0x2082080000000 false
        (natWord_step 9 54 0x1041040000000 false
        (natWord_step 9 53 0x820820000000 false
        (natWord_step 9 52 0x410410000000 false
        (natWord_step 9 51 0x208208000000 false
        (natWord_step 9 50 0x104104000000 false
        (natWord_step 9 49 0x82082000000 false
        (natWord_step 9 48 0x41041000000 false
        (natWord_step 9 47 0x20820800000 false
        (natWord_step 9 46 0x10410400000 false
        (natWord_step 9 45 0x8208200000 false
        (natWord_step 9 44 0x4104100000 false
        (natWord_step 9 43 0x2082080000 false
        (natWord_step 9 42 0x1041040000 false
        (natWord_step 9 41 0x820820000 false
        (natWord_step 9 40 0x410410000 false
        (natWord_step 9 39 0x208208000 false
        (natWord_step 9 38 0x104104000 false
        (natWord_step 9 37 0x82082000 false
        (natWord_step 9 36 0x41041000 false
        (natWord_step 9 35 0x20820800 false
        (natWord_step 9 34 0x10410400 false
        (natWord_step 9 33 0x8208200 false
        (natWord_step 9 32 0x4104100 false
        (natWord_step 9 31 0x2082080 false
        (natWord_step 9 30 0x1041040 false
        (natWord_step 9 29 0x820820 false
        (natWord_step 9 28 0x410410 false
        (natWord_step 9 27 0x208208 false
        (natWord_step 9 26 0x104104 false
        (natWord_step 9 25 0x82082 false
        (natWord_step 9 24 0x41041 false
        (natWord_step 9 23 0x20820 true
        (natWord_step 9 22 0x10410 false
        (natWord_step 9 21 0x8208 false
        (natWord_step 9 20 0x4104 false
        (natWord_step 9 19 0x2082 false
        (natWord_step 9 18 0x1041 false
        (natWord_step 9 17 0x820 true
        (natWord_step 9 16 0x410 false
        (natWord_step 9 15 0x208 false
        (natWord_step 9 14 0x104 false
        (natWord_step 9 13 0x82 false
        (natWord_step 9 12 0x41 false
        (natWord_step 9 11 0x20 true
        (natWord_step 9 10 0x10 false
        (natWord_step 9 9 0x8 false
        (natWord_step 9 8 0x4 false
        (natWord_step 9 7 0x2 false
        (natWord_step 9 6 0x1 false
        (natWord_step 9 5 0x0 true
        (natWord_step 9 4 0x0 false
        (natWord_step 9 3 0x0 false
        (natWord_step 9 2 0x0 false
        (natWord_step 9 1 0x0 false
        (natWord_step 9 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 576 = false))
        (by decide : natBit 577 = false))
        (by decide : natBit 578 = false))
        (by decide : natBit 579 = false))
        (by decide : natBit 580 = false))
        (by decide : natBit 581 = true))
        (by decide : natBit 582 = false))
        (by decide : natBit 583 = false))
        (by decide : natBit 584 = false))
        (by decide : natBit 585 = false))
        (by decide : natBit 586 = false))
        (by decide : natBit 587 = true))
        (by decide : natBit 588 = false))
        (by decide : natBit 589 = false))
        (by decide : natBit 590 = false))
        (by decide : natBit 591 = false))
        (by decide : natBit 592 = false))
        (by decide : natBit 593 = true))
        (by decide : natBit 594 = false))
        (by decide : natBit 595 = false))
        (by decide : natBit 596 = false))
        (by decide : natBit 597 = false))
        (by decide : natBit 598 = false))
        (by decide : natBit 599 = true))
        (by decide : natBit 600 = false))
        (by decide : natBit 601 = false))
        (by decide : natBit 602 = false))
        (by decide : natBit 603 = false))
        (by decide : natBit 604 = false))
        (by decide : natBit 605 = false))
        (by decide : natBit 606 = false))
        (by decide : natBit 607 = false))
        (by decide : natBit 608 = false))
        (by decide : natBit 609 = false))
        (by decide : natBit 610 = false))
        (by decide : natBit 611 = false))
        (by decide : natBit 612 = false))
        (by decide : natBit 613 = false))
        (by decide : natBit 614 = false))
        (by decide : natBit 615 = false))
        (by decide : natBit 616 = false))
        (by decide : natBit 617 = false))
        (by decide : natBit 618 = false))
        (by decide : natBit 619 = false))
        (by decide : natBit 620 = false))
        (by decide : natBit 621 = false))
        (by decide : natBit 622 = false))
        (by decide : natBit 623 = false))
        (by decide : natBit 624 = false))
        (by decide : natBit 625 = false))
        (by decide : natBit 626 = false))
        (by decide : natBit 627 = false))
        (by decide : natBit 628 = false))
        (by decide : natBit 629 = false))
        (by decide : natBit 630 = false))
        (by decide : natBit 631 = false))
        (by decide : natBit 632 = false))
        (by decide : natBit 633 = false))
        (by decide : natBit 634 = false))
        (by decide : natBit 635 = false))
        (by decide : natBit 636 = false))
        (by decide : natBit 637 = false))
        (by decide : natBit 638 = false))
        (by decide : natBit 639 = false))

    theorem nat_word_9 : wordWith room_as_nat 9 64 = 0x410410000000000 :=
      Eq.trans (word_reads_nat 9 64) nat_word_9_calculated

    theorem nat_word_10_calculated : natWord 10 64 = 0x1c30c30400c30c :=
      (natWord_step 10 63 0xe186182006186 false
        (natWord_step 10 62 0x70c30c10030c3 false
        (natWord_step 10 61 0x3861860801861 true
        (natWord_step 10 60 0x1c30c30400c30 true
        (natWord_step 10 59 0xe18618200618 false
        (natWord_step 10 58 0x70c30c10030c false
        (natWord_step 10 57 0x386186080186 false
        (natWord_step 10 56 0x1c30c30400c3 false
        (natWord_step 10 55 0xe1861820061 true
        (natWord_step 10 54 0x70c30c10030 true
        (natWord_step 10 53 0x38618608018 false
        (natWord_step 10 52 0x1c30c30400c false
        (natWord_step 10 51 0xe186182006 false
        (natWord_step 10 50 0x70c30c1003 false
        (natWord_step 10 49 0x3861860801 true
        (natWord_step 10 48 0x1c30c30400 true
        (natWord_step 10 47 0xe18618200 false
        (natWord_step 10 46 0x70c30c100 false
        (natWord_step 10 45 0x386186080 false
        (natWord_step 10 44 0x1c30c3040 false
        (natWord_step 10 43 0xe1861820 false
        (natWord_step 10 42 0x70c30c10 false
        (natWord_step 10 41 0x38618608 false
        (natWord_step 10 40 0x1c30c304 false
        (natWord_step 10 39 0xe186182 false
        (natWord_step 10 38 0x70c30c1 false
        (natWord_step 10 37 0x3861860 true
        (natWord_step 10 36 0x1c30c30 false
        (natWord_step 10 35 0xe18618 false
        (natWord_step 10 34 0x70c30c false
        (natWord_step 10 33 0x386186 false
        (natWord_step 10 32 0x1c30c3 false
        (natWord_step 10 31 0xe1861 true
        (natWord_step 10 30 0x70c30 true
        (natWord_step 10 29 0x38618 false
        (natWord_step 10 28 0x1c30c false
        (natWord_step 10 27 0xe186 false
        (natWord_step 10 26 0x70c3 false
        (natWord_step 10 25 0x3861 true
        (natWord_step 10 24 0x1c30 true
        (natWord_step 10 23 0xe18 false
        (natWord_step 10 22 0x70c false
        (natWord_step 10 21 0x386 false
        (natWord_step 10 20 0x1c3 false
        (natWord_step 10 19 0xe1 true
        (natWord_step 10 18 0x70 true
        (natWord_step 10 17 0x38 false
        (natWord_step 10 16 0x1c false
        (natWord_step 10 15 0xe false
        (natWord_step 10 14 0x7 false
        (natWord_step 10 13 0x3 true
        (natWord_step 10 12 0x1 true
        (natWord_step 10 11 0x0 true
        (natWord_step 10 10 0x0 false
        (natWord_step 10 9 0x0 false
        (natWord_step 10 8 0x0 false
        (natWord_step 10 7 0x0 false
        (natWord_step 10 6 0x0 false
        (natWord_step 10 5 0x0 false
        (natWord_step 10 4 0x0 false
        (natWord_step 10 3 0x0 false
        (natWord_step 10 2 0x0 false
        (natWord_step 10 1 0x0 false
        (natWord_step 10 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 640 = false))
        (by decide : natBit 641 = false))
        (by decide : natBit 642 = false))
        (by decide : natBit 643 = false))
        (by decide : natBit 644 = false))
        (by decide : natBit 645 = false))
        (by decide : natBit 646 = false))
        (by decide : natBit 647 = false))
        (by decide : natBit 648 = false))
        (by decide : natBit 649 = false))
        (by decide : natBit 650 = false))
        (by decide : natBit 651 = true))
        (by decide : natBit 652 = true))
        (by decide : natBit 653 = true))
        (by decide : natBit 654 = false))
        (by decide : natBit 655 = false))
        (by decide : natBit 656 = false))
        (by decide : natBit 657 = false))
        (by decide : natBit 658 = true))
        (by decide : natBit 659 = true))
        (by decide : natBit 660 = false))
        (by decide : natBit 661 = false))
        (by decide : natBit 662 = false))
        (by decide : natBit 663 = false))
        (by decide : natBit 664 = true))
        (by decide : natBit 665 = true))
        (by decide : natBit 666 = false))
        (by decide : natBit 667 = false))
        (by decide : natBit 668 = false))
        (by decide : natBit 669 = false))
        (by decide : natBit 670 = true))
        (by decide : natBit 671 = true))
        (by decide : natBit 672 = false))
        (by decide : natBit 673 = false))
        (by decide : natBit 674 = false))
        (by decide : natBit 675 = false))
        (by decide : natBit 676 = false))
        (by decide : natBit 677 = true))
        (by decide : natBit 678 = false))
        (by decide : natBit 679 = false))
        (by decide : natBit 680 = false))
        (by decide : natBit 681 = false))
        (by decide : natBit 682 = false))
        (by decide : natBit 683 = false))
        (by decide : natBit 684 = false))
        (by decide : natBit 685 = false))
        (by decide : natBit 686 = false))
        (by decide : natBit 687 = false))
        (by decide : natBit 688 = true))
        (by decide : natBit 689 = true))
        (by decide : natBit 690 = false))
        (by decide : natBit 691 = false))
        (by decide : natBit 692 = false))
        (by decide : natBit 693 = false))
        (by decide : natBit 694 = true))
        (by decide : natBit 695 = true))
        (by decide : natBit 696 = false))
        (by decide : natBit 697 = false))
        (by decide : natBit 698 = false))
        (by decide : natBit 699 = false))
        (by decide : natBit 700 = true))
        (by decide : natBit 701 = true))
        (by decide : natBit 702 = false))
        (by decide : natBit 703 = false))

    theorem nat_word_10 : wordWith room_as_nat 10 64 = 0x1c30c30400c30c :=
      Eq.trans (word_reads_nat 10 64) nat_word_10_calculated

    theorem nat_word_11_calculated : natWord 11 64 = 0x30400c30c30400c3 :=
      (natWord_step 11 63 0x1820061861820061 true
        (natWord_step 11 62 0xc10030c30c10030 true
        (natWord_step 11 61 0x608018618608018 false
        (natWord_step 11 60 0x30400c30c30400c false
        (natWord_step 11 59 0x182006186182006 false
        (natWord_step 11 58 0xc10030c30c1003 false
        (natWord_step 11 57 0x60801861860801 true
        (natWord_step 11 56 0x30400c30c30400 true
        (natWord_step 11 55 0x18200618618200 false
        (natWord_step 11 54 0xc10030c30c100 false
        (natWord_step 11 53 0x6080186186080 false
        (natWord_step 11 52 0x30400c30c3040 false
        (natWord_step 11 51 0x1820061861820 false
        (natWord_step 11 50 0xc10030c30c10 false
        (natWord_step 11 49 0x608018618608 false
        (natWord_step 11 48 0x30400c30c304 false
        (natWord_step 11 47 0x182006186182 false
        (natWord_step 11 46 0xc10030c30c1 false
        (natWord_step 11 45 0x60801861860 true
        (natWord_step 11 44 0x30400c30c30 false
        (natWord_step 11 43 0x18200618618 false
        (natWord_step 11 42 0xc10030c30c false
        (natWord_step 11 41 0x6080186186 false
        (natWord_step 11 40 0x30400c30c3 false
        (natWord_step 11 39 0x1820061861 true
        (natWord_step 11 38 0xc10030c30 true
        (natWord_step 11 37 0x608018618 false
        (natWord_step 11 36 0x30400c30c false
        (natWord_step 11 35 0x182006186 false
        (natWord_step 11 34 0xc10030c3 false
        (natWord_step 11 33 0x60801861 true
        (natWord_step 11 32 0x30400c30 true
        (natWord_step 11 31 0x18200618 false
        (natWord_step 11 30 0xc10030c false
        (natWord_step 11 29 0x6080186 false
        (natWord_step 11 28 0x30400c3 false
        (natWord_step 11 27 0x1820061 true
        (natWord_step 11 26 0xc10030 true
        (natWord_step 11 25 0x608018 false
        (natWord_step 11 24 0x30400c false
        (natWord_step 11 23 0x182006 false
        (natWord_step 11 22 0xc1003 false
        (natWord_step 11 21 0x60801 true
        (natWord_step 11 20 0x30400 true
        (natWord_step 11 19 0x18200 false
        (natWord_step 11 18 0xc100 false
        (natWord_step 11 17 0x6080 false
        (natWord_step 11 16 0x3040 false
        (natWord_step 11 15 0x1820 false
        (natWord_step 11 14 0xc10 false
        (natWord_step 11 13 0x608 false
        (natWord_step 11 12 0x304 false
        (natWord_step 11 11 0x182 false
        (natWord_step 11 10 0xc1 false
        (natWord_step 11 9 0x60 true
        (natWord_step 11 8 0x30 false
        (natWord_step 11 7 0x18 false
        (natWord_step 11 6 0xc false
        (natWord_step 11 5 0x6 false
        (natWord_step 11 4 0x3 false
        (natWord_step 11 3 0x1 true
        (natWord_step 11 2 0x0 true
        (natWord_step 11 1 0x0 false
        (natWord_step 11 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 704 = false))
        (by decide : natBit 705 = false))
        (by decide : natBit 706 = true))
        (by decide : natBit 707 = true))
        (by decide : natBit 708 = false))
        (by decide : natBit 709 = false))
        (by decide : natBit 710 = false))
        (by decide : natBit 711 = false))
        (by decide : natBit 712 = false))
        (by decide : natBit 713 = true))
        (by decide : natBit 714 = false))
        (by decide : natBit 715 = false))
        (by decide : natBit 716 = false))
        (by decide : natBit 717 = false))
        (by decide : natBit 718 = false))
        (by decide : natBit 719 = false))
        (by decide : natBit 720 = false))
        (by decide : natBit 721 = false))
        (by decide : natBit 722 = false))
        (by decide : natBit 723 = false))
        (by decide : natBit 724 = true))
        (by decide : natBit 725 = true))
        (by decide : natBit 726 = false))
        (by decide : natBit 727 = false))
        (by decide : natBit 728 = false))
        (by decide : natBit 729 = false))
        (by decide : natBit 730 = true))
        (by decide : natBit 731 = true))
        (by decide : natBit 732 = false))
        (by decide : natBit 733 = false))
        (by decide : natBit 734 = false))
        (by decide : natBit 735 = false))
        (by decide : natBit 736 = true))
        (by decide : natBit 737 = true))
        (by decide : natBit 738 = false))
        (by decide : natBit 739 = false))
        (by decide : natBit 740 = false))
        (by decide : natBit 741 = false))
        (by decide : natBit 742 = true))
        (by decide : natBit 743 = true))
        (by decide : natBit 744 = false))
        (by decide : natBit 745 = false))
        (by decide : natBit 746 = false))
        (by decide : natBit 747 = false))
        (by decide : natBit 748 = false))
        (by decide : natBit 749 = true))
        (by decide : natBit 750 = false))
        (by decide : natBit 751 = false))
        (by decide : natBit 752 = false))
        (by decide : natBit 753 = false))
        (by decide : natBit 754 = false))
        (by decide : natBit 755 = false))
        (by decide : natBit 756 = false))
        (by decide : natBit 757 = false))
        (by decide : natBit 758 = false))
        (by decide : natBit 759 = false))
        (by decide : natBit 760 = true))
        (by decide : natBit 761 = true))
        (by decide : natBit 762 = false))
        (by decide : natBit 763 = false))
        (by decide : natBit 764 = false))
        (by decide : natBit 765 = false))
        (by decide : natBit 766 = true))
        (by decide : natBit 767 = true))

    theorem nat_word_11 : wordWith room_as_nat 11 64 = 0x30400c30c30400c3 :=
      Eq.trans (word_reads_nat 11 64) nat_word_11_calculated

    theorem nat_word_12_calculated : natWord 12 64 = 0xc30400410410000 :=
      (natWord_step 12 63 0x618200208208000 false
        (natWord_step 12 62 0x30c100104104000 false
        (natWord_step 12 61 0x186080082082000 false
        (natWord_step 12 60 0xc3040041041000 false
        (natWord_step 12 59 0x61820020820800 false
        (natWord_step 12 58 0x30c10010410400 false
        (natWord_step 12 57 0x18608008208200 false
        (natWord_step 12 56 0xc304004104100 false
        (natWord_step 12 55 0x6182002082080 false
        (natWord_step 12 54 0x30c1001041040 false
        (natWord_step 12 53 0x1860800820820 false
        (natWord_step 12 52 0xc30400410410 false
        (natWord_step 12 51 0x618200208208 false
        (natWord_step 12 50 0x30c100104104 false
        (natWord_step 12 49 0x186080082082 false
        (natWord_step 12 48 0xc3040041041 false
        (natWord_step 12 47 0x61820020820 true
        (natWord_step 12 46 0x30c10010410 false
        (natWord_step 12 45 0x18608008208 false
        (natWord_step 12 44 0xc304004104 false
        (natWord_step 12 43 0x6182002082 false
        (natWord_step 12 42 0x30c1001041 false
        (natWord_step 12 41 0x1860800820 true
        (natWord_step 12 40 0xc30400410 false
        (natWord_step 12 39 0x618200208 false
        (natWord_step 12 38 0x30c100104 false
        (natWord_step 12 37 0x186080082 false
        (natWord_step 12 36 0xc3040041 false
        (natWord_step 12 35 0x61820020 true
        (natWord_step 12 34 0x30c10010 false
        (natWord_step 12 33 0x18608008 false
        (natWord_step 12 32 0xc304004 false
        (natWord_step 12 31 0x6182002 false
        (natWord_step 12 30 0x30c1001 false
        (natWord_step 12 29 0x1860800 true
        (natWord_step 12 28 0xc30400 false
        (natWord_step 12 27 0x618200 false
        (natWord_step 12 26 0x30c100 false
        (natWord_step 12 25 0x186080 false
        (natWord_step 12 24 0xc3040 false
        (natWord_step 12 23 0x61820 false
        (natWord_step 12 22 0x30c10 false
        (natWord_step 12 21 0x18608 false
        (natWord_step 12 20 0xc304 false
        (natWord_step 12 19 0x6182 false
        (natWord_step 12 18 0x30c1 false
        (natWord_step 12 17 0x1860 true
        (natWord_step 12 16 0xc30 false
        (natWord_step 12 15 0x618 false
        (natWord_step 12 14 0x30c false
        (natWord_step 12 13 0x186 false
        (natWord_step 12 12 0xc3 false
        (natWord_step 12 11 0x61 true
        (natWord_step 12 10 0x30 true
        (natWord_step 12 9 0x18 false
        (natWord_step 12 8 0xc false
        (natWord_step 12 7 0x6 false
        (natWord_step 12 6 0x3 false
        (natWord_step 12 5 0x1 true
        (natWord_step 12 4 0x0 true
        (natWord_step 12 3 0x0 false
        (natWord_step 12 2 0x0 false
        (natWord_step 12 1 0x0 false
        (natWord_step 12 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 768 = false))
        (by decide : natBit 769 = false))
        (by decide : natBit 770 = false))
        (by decide : natBit 771 = false))
        (by decide : natBit 772 = true))
        (by decide : natBit 773 = true))
        (by decide : natBit 774 = false))
        (by decide : natBit 775 = false))
        (by decide : natBit 776 = false))
        (by decide : natBit 777 = false))
        (by decide : natBit 778 = true))
        (by decide : natBit 779 = true))
        (by decide : natBit 780 = false))
        (by decide : natBit 781 = false))
        (by decide : natBit 782 = false))
        (by decide : natBit 783 = false))
        (by decide : natBit 784 = false))
        (by decide : natBit 785 = true))
        (by decide : natBit 786 = false))
        (by decide : natBit 787 = false))
        (by decide : natBit 788 = false))
        (by decide : natBit 789 = false))
        (by decide : natBit 790 = false))
        (by decide : natBit 791 = false))
        (by decide : natBit 792 = false))
        (by decide : natBit 793 = false))
        (by decide : natBit 794 = false))
        (by decide : natBit 795 = false))
        (by decide : natBit 796 = false))
        (by decide : natBit 797 = true))
        (by decide : natBit 798 = false))
        (by decide : natBit 799 = false))
        (by decide : natBit 800 = false))
        (by decide : natBit 801 = false))
        (by decide : natBit 802 = false))
        (by decide : natBit 803 = true))
        (by decide : natBit 804 = false))
        (by decide : natBit 805 = false))
        (by decide : natBit 806 = false))
        (by decide : natBit 807 = false))
        (by decide : natBit 808 = false))
        (by decide : natBit 809 = true))
        (by decide : natBit 810 = false))
        (by decide : natBit 811 = false))
        (by decide : natBit 812 = false))
        (by decide : natBit 813 = false))
        (by decide : natBit 814 = false))
        (by decide : natBit 815 = true))
        (by decide : natBit 816 = false))
        (by decide : natBit 817 = false))
        (by decide : natBit 818 = false))
        (by decide : natBit 819 = false))
        (by decide : natBit 820 = false))
        (by decide : natBit 821 = false))
        (by decide : natBit 822 = false))
        (by decide : natBit 823 = false))
        (by decide : natBit 824 = false))
        (by decide : natBit 825 = false))
        (by decide : natBit 826 = false))
        (by decide : natBit 827 = false))
        (by decide : natBit 828 = false))
        (by decide : natBit 829 = false))
        (by decide : natBit 830 = false))
        (by decide : natBit 831 = false))

    theorem nat_word_12 : wordWith room_as_nat 12 64 = 0xc30400410410000 :=
      Eq.trans (word_reads_nat 12 64) nat_word_12_calculated

    theorem nat_word_13_calculated : natWord 13 64 = 0xc104100 :=
      (natWord_step 13 63 0x6082080 false
        (natWord_step 13 62 0x3041040 false
        (natWord_step 13 61 0x1820820 false
        (natWord_step 13 60 0xc10410 false
        (natWord_step 13 59 0x608208 false
        (natWord_step 13 58 0x304104 false
        (natWord_step 13 57 0x182082 false
        (natWord_step 13 56 0xc1041 false
        (natWord_step 13 55 0x60820 true
        (natWord_step 13 54 0x30410 false
        (natWord_step 13 53 0x18208 false
        (natWord_step 13 52 0xc104 false
        (natWord_step 13 51 0x6082 false
        (natWord_step 13 50 0x3041 false
        (natWord_step 13 49 0x1820 true
        (natWord_step 13 48 0xc10 false
        (natWord_step 13 47 0x608 false
        (natWord_step 13 46 0x304 false
        (natWord_step 13 45 0x182 false
        (natWord_step 13 44 0xc1 false
        (natWord_step 13 43 0x60 true
        (natWord_step 13 42 0x30 false
        (natWord_step 13 41 0x18 false
        (natWord_step 13 40 0xc false
        (natWord_step 13 39 0x6 false
        (natWord_step 13 38 0x3 false
        (natWord_step 13 37 0x1 true
        (natWord_step 13 36 0x0 true
        (natWord_step 13 35 0x0 false
        (natWord_step 13 34 0x0 false
        (natWord_step 13 33 0x0 false
        (natWord_step 13 32 0x0 false
        (natWord_step 13 31 0x0 false
        (natWord_step 13 30 0x0 false
        (natWord_step 13 29 0x0 false
        (natWord_step 13 28 0x0 false
        (natWord_step 13 27 0x0 false
        (natWord_step 13 26 0x0 false
        (natWord_step 13 25 0x0 false
        (natWord_step 13 24 0x0 false
        (natWord_step 13 23 0x0 false
        (natWord_step 13 22 0x0 false
        (natWord_step 13 21 0x0 false
        (natWord_step 13 20 0x0 false
        (natWord_step 13 19 0x0 false
        (natWord_step 13 18 0x0 false
        (natWord_step 13 17 0x0 false
        (natWord_step 13 16 0x0 false
        (natWord_step 13 15 0x0 false
        (natWord_step 13 14 0x0 false
        (natWord_step 13 13 0x0 false
        (natWord_step 13 12 0x0 false
        (natWord_step 13 11 0x0 false
        (natWord_step 13 10 0x0 false
        (natWord_step 13 9 0x0 false
        (natWord_step 13 8 0x0 false
        (natWord_step 13 7 0x0 false
        (natWord_step 13 6 0x0 false
        (natWord_step 13 5 0x0 false
        (natWord_step 13 4 0x0 false
        (natWord_step 13 3 0x0 false
        (natWord_step 13 2 0x0 false
        (natWord_step 13 1 0x0 false
        (natWord_step 13 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 832 = false))
        (by decide : natBit 833 = false))
        (by decide : natBit 834 = false))
        (by decide : natBit 835 = false))
        (by decide : natBit 836 = false))
        (by decide : natBit 837 = false))
        (by decide : natBit 838 = false))
        (by decide : natBit 839 = false))
        (by decide : natBit 840 = false))
        (by decide : natBit 841 = false))
        (by decide : natBit 842 = false))
        (by decide : natBit 843 = false))
        (by decide : natBit 844 = false))
        (by decide : natBit 845 = false))
        (by decide : natBit 846 = false))
        (by decide : natBit 847 = false))
        (by decide : natBit 848 = false))
        (by decide : natBit 849 = false))
        (by decide : natBit 850 = false))
        (by decide : natBit 851 = false))
        (by decide : natBit 852 = false))
        (by decide : natBit 853 = false))
        (by decide : natBit 854 = false))
        (by decide : natBit 855 = false))
        (by decide : natBit 856 = false))
        (by decide : natBit 857 = false))
        (by decide : natBit 858 = false))
        (by decide : natBit 859 = false))
        (by decide : natBit 860 = false))
        (by decide : natBit 861 = false))
        (by decide : natBit 862 = false))
        (by decide : natBit 863 = false))
        (by decide : natBit 864 = false))
        (by decide : natBit 865 = false))
        (by decide : natBit 866 = false))
        (by decide : natBit 867 = false))
        (by decide : natBit 868 = true))
        (by decide : natBit 869 = true))
        (by decide : natBit 870 = false))
        (by decide : natBit 871 = false))
        (by decide : natBit 872 = false))
        (by decide : natBit 873 = false))
        (by decide : natBit 874 = false))
        (by decide : natBit 875 = true))
        (by decide : natBit 876 = false))
        (by decide : natBit 877 = false))
        (by decide : natBit 878 = false))
        (by decide : natBit 879 = false))
        (by decide : natBit 880 = false))
        (by decide : natBit 881 = true))
        (by decide : natBit 882 = false))
        (by decide : natBit 883 = false))
        (by decide : natBit 884 = false))
        (by decide : natBit 885 = false))
        (by decide : natBit 886 = false))
        (by decide : natBit 887 = true))
        (by decide : natBit 888 = false))
        (by decide : natBit 889 = false))
        (by decide : natBit 890 = false))
        (by decide : natBit 891 = false))
        (by decide : natBit 892 = false))
        (by decide : natBit 893 = false))
        (by decide : natBit 894 = false))
        (by decide : natBit 895 = false))

    theorem nat_word_13 : wordWith room_as_nat 13 64 = 0xc104100 :=
      Eq.trans (word_reads_nat 13 64) nat_word_13_calculated

    theorem nat_word_14_calculated : natWord 14 64 = 0x41041000041041 :=
      (natWord_step 14 63 0x20820800020820 true
        (natWord_step 14 62 0x10410400010410 false
        (natWord_step 14 61 0x8208200008208 false
        (natWord_step 14 60 0x4104100004104 false
        (natWord_step 14 59 0x2082080002082 false
        (natWord_step 14 58 0x1041040001041 false
        (natWord_step 14 57 0x820820000820 true
        (natWord_step 14 56 0x410410000410 false
        (natWord_step 14 55 0x208208000208 false
        (natWord_step 14 54 0x104104000104 false
        (natWord_step 14 53 0x82082000082 false
        (natWord_step 14 52 0x41041000041 false
        (natWord_step 14 51 0x20820800020 true
        (natWord_step 14 50 0x10410400010 false
        (natWord_step 14 49 0x8208200008 false
        (natWord_step 14 48 0x4104100004 false
        (natWord_step 14 47 0x2082080002 false
        (natWord_step 14 46 0x1041040001 false
        (natWord_step 14 45 0x820820000 true
        (natWord_step 14 44 0x410410000 false
        (natWord_step 14 43 0x208208000 false
        (natWord_step 14 42 0x104104000 false
        (natWord_step 14 41 0x82082000 false
        (natWord_step 14 40 0x41041000 false
        (natWord_step 14 39 0x20820800 false
        (natWord_step 14 38 0x10410400 false
        (natWord_step 14 37 0x8208200 false
        (natWord_step 14 36 0x4104100 false
        (natWord_step 14 35 0x2082080 false
        (natWord_step 14 34 0x1041040 false
        (natWord_step 14 33 0x820820 false
        (natWord_step 14 32 0x410410 false
        (natWord_step 14 31 0x208208 false
        (natWord_step 14 30 0x104104 false
        (natWord_step 14 29 0x82082 false
        (natWord_step 14 28 0x41041 false
        (natWord_step 14 27 0x20820 true
        (natWord_step 14 26 0x10410 false
        (natWord_step 14 25 0x8208 false
        (natWord_step 14 24 0x4104 false
        (natWord_step 14 23 0x2082 false
        (natWord_step 14 22 0x1041 false
        (natWord_step 14 21 0x820 true
        (natWord_step 14 20 0x410 false
        (natWord_step 14 19 0x208 false
        (natWord_step 14 18 0x104 false
        (natWord_step 14 17 0x82 false
        (natWord_step 14 16 0x41 false
        (natWord_step 14 15 0x20 true
        (natWord_step 14 14 0x10 false
        (natWord_step 14 13 0x8 false
        (natWord_step 14 12 0x4 false
        (natWord_step 14 11 0x2 false
        (natWord_step 14 10 0x1 false
        (natWord_step 14 9 0x0 true
        (natWord_step 14 8 0x0 false
        (natWord_step 14 7 0x0 false
        (natWord_step 14 6 0x0 false
        (natWord_step 14 5 0x0 false
        (natWord_step 14 4 0x0 false
        (natWord_step 14 3 0x0 false
        (natWord_step 14 2 0x0 false
        (natWord_step 14 1 0x0 false
        (natWord_step 14 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 896 = false))
        (by decide : natBit 897 = false))
        (by decide : natBit 898 = false))
        (by decide : natBit 899 = false))
        (by decide : natBit 900 = false))
        (by decide : natBit 901 = false))
        (by decide : natBit 902 = false))
        (by decide : natBit 903 = false))
        (by decide : natBit 904 = false))
        (by decide : natBit 905 = true))
        (by decide : natBit 906 = false))
        (by decide : natBit 907 = false))
        (by decide : natBit 908 = false))
        (by decide : natBit 909 = false))
        (by decide : natBit 910 = false))
        (by decide : natBit 911 = true))
        (by decide : natBit 912 = false))
        (by decide : natBit 913 = false))
        (by decide : natBit 914 = false))
        (by decide : natBit 915 = false))
        (by decide : natBit 916 = false))
        (by decide : natBit 917 = true))
        (by decide : natBit 918 = false))
        (by decide : natBit 919 = false))
        (by decide : natBit 920 = false))
        (by decide : natBit 921 = false))
        (by decide : natBit 922 = false))
        (by decide : natBit 923 = true))
        (by decide : natBit 924 = false))
        (by decide : natBit 925 = false))
        (by decide : natBit 926 = false))
        (by decide : natBit 927 = false))
        (by decide : natBit 928 = false))
        (by decide : natBit 929 = false))
        (by decide : natBit 930 = false))
        (by decide : natBit 931 = false))
        (by decide : natBit 932 = false))
        (by decide : natBit 933 = false))
        (by decide : natBit 934 = false))
        (by decide : natBit 935 = false))
        (by decide : natBit 936 = false))
        (by decide : natBit 937 = false))
        (by decide : natBit 938 = false))
        (by decide : natBit 939 = false))
        (by decide : natBit 940 = false))
        (by decide : natBit 941 = true))
        (by decide : natBit 942 = false))
        (by decide : natBit 943 = false))
        (by decide : natBit 944 = false))
        (by decide : natBit 945 = false))
        (by decide : natBit 946 = false))
        (by decide : natBit 947 = true))
        (by decide : natBit 948 = false))
        (by decide : natBit 949 = false))
        (by decide : natBit 950 = false))
        (by decide : natBit 951 = false))
        (by decide : natBit 952 = false))
        (by decide : natBit 953 = true))
        (by decide : natBit 954 = false))
        (by decide : natBit 955 = false))
        (by decide : natBit 956 = false))
        (by decide : natBit 957 = false))
        (by decide : natBit 958 = false))
        (by decide : natBit 959 = true))

    theorem nat_word_14 : wordWith room_as_nat 14 64 = 0x41041000041041 :=
      Eq.trans (word_reads_nat 14 64) nat_word_14_calculated

    theorem nat_word_15_calculated : natWord 15 64 = 0x410410000000 :=
      (natWord_step 15 63 0x208208000000 false
        (natWord_step 15 62 0x104104000000 false
        (natWord_step 15 61 0x82082000000 false
        (natWord_step 15 60 0x41041000000 false
        (natWord_step 15 59 0x20820800000 false
        (natWord_step 15 58 0x10410400000 false
        (natWord_step 15 57 0x8208200000 false
        (natWord_step 15 56 0x4104100000 false
        (natWord_step 15 55 0x2082080000 false
        (natWord_step 15 54 0x1041040000 false
        (natWord_step 15 53 0x820820000 false
        (natWord_step 15 52 0x410410000 false
        (natWord_step 15 51 0x208208000 false
        (natWord_step 15 50 0x104104000 false
        (natWord_step 15 49 0x82082000 false
        (natWord_step 15 48 0x41041000 false
        (natWord_step 15 47 0x20820800 false
        (natWord_step 15 46 0x10410400 false
        (natWord_step 15 45 0x8208200 false
        (natWord_step 15 44 0x4104100 false
        (natWord_step 15 43 0x2082080 false
        (natWord_step 15 42 0x1041040 false
        (natWord_step 15 41 0x820820 false
        (natWord_step 15 40 0x410410 false
        (natWord_step 15 39 0x208208 false
        (natWord_step 15 38 0x104104 false
        (natWord_step 15 37 0x82082 false
        (natWord_step 15 36 0x41041 false
        (natWord_step 15 35 0x20820 true
        (natWord_step 15 34 0x10410 false
        (natWord_step 15 33 0x8208 false
        (natWord_step 15 32 0x4104 false
        (natWord_step 15 31 0x2082 false
        (natWord_step 15 30 0x1041 false
        (natWord_step 15 29 0x820 true
        (natWord_step 15 28 0x410 false
        (natWord_step 15 27 0x208 false
        (natWord_step 15 26 0x104 false
        (natWord_step 15 25 0x82 false
        (natWord_step 15 24 0x41 false
        (natWord_step 15 23 0x20 true
        (natWord_step 15 22 0x10 false
        (natWord_step 15 21 0x8 false
        (natWord_step 15 20 0x4 false
        (natWord_step 15 19 0x2 false
        (natWord_step 15 18 0x1 false
        (natWord_step 15 17 0x0 true
        (natWord_step 15 16 0x0 false
        (natWord_step 15 15 0x0 false
        (natWord_step 15 14 0x0 false
        (natWord_step 15 13 0x0 false
        (natWord_step 15 12 0x0 false
        (natWord_step 15 11 0x0 false
        (natWord_step 15 10 0x0 false
        (natWord_step 15 9 0x0 false
        (natWord_step 15 8 0x0 false
        (natWord_step 15 7 0x0 false
        (natWord_step 15 6 0x0 false
        (natWord_step 15 5 0x0 false
        (natWord_step 15 4 0x0 false
        (natWord_step 15 3 0x0 false
        (natWord_step 15 2 0x0 false
        (natWord_step 15 1 0x0 false
        (natWord_step 15 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 960 = false))
        (by decide : natBit 961 = false))
        (by decide : natBit 962 = false))
        (by decide : natBit 963 = false))
        (by decide : natBit 964 = false))
        (by decide : natBit 965 = false))
        (by decide : natBit 966 = false))
        (by decide : natBit 967 = false))
        (by decide : natBit 968 = false))
        (by decide : natBit 969 = false))
        (by decide : natBit 970 = false))
        (by decide : natBit 971 = false))
        (by decide : natBit 972 = false))
        (by decide : natBit 973 = false))
        (by decide : natBit 974 = false))
        (by decide : natBit 975 = false))
        (by decide : natBit 976 = false))
        (by decide : natBit 977 = true))
        (by decide : natBit 978 = false))
        (by decide : natBit 979 = false))
        (by decide : natBit 980 = false))
        (by decide : natBit 981 = false))
        (by decide : natBit 982 = false))
        (by decide : natBit 983 = true))
        (by decide : natBit 984 = false))
        (by decide : natBit 985 = false))
        (by decide : natBit 986 = false))
        (by decide : natBit 987 = false))
        (by decide : natBit 988 = false))
        (by decide : natBit 989 = true))
        (by decide : natBit 990 = false))
        (by decide : natBit 991 = false))
        (by decide : natBit 992 = false))
        (by decide : natBit 993 = false))
        (by decide : natBit 994 = false))
        (by decide : natBit 995 = true))
        (by decide : natBit 996 = false))
        (by decide : natBit 997 = false))
        (by decide : natBit 998 = false))
        (by decide : natBit 999 = false))
        (by decide : natBit 1000 = false))
        (by decide : natBit 1001 = false))
        (by decide : natBit 1002 = false))
        (by decide : natBit 1003 = false))
        (by decide : natBit 1004 = false))
        (by decide : natBit 1005 = false))
        (by decide : natBit 1006 = false))
        (by decide : natBit 1007 = false))
        (by decide : natBit 1008 = false))
        (by decide : natBit 1009 = false))
        (by decide : natBit 1010 = false))
        (by decide : natBit 1011 = false))
        (by decide : natBit 1012 = false))
        (by decide : natBit 1013 = false))
        (by decide : natBit 1014 = false))
        (by decide : natBit 1015 = false))
        (by decide : natBit 1016 = false))
        (by decide : natBit 1017 = false))
        (by decide : natBit 1018 = false))
        (by decide : natBit 1019 = false))
        (by decide : natBit 1020 = false))
        (by decide : natBit 1021 = false))
        (by decide : natBit 1022 = false))
        (by decide : natBit 1023 = false))

    theorem nat_word_15 : wordWith room_as_nat 15 64 = 0x410410000000 :=
      Eq.trans (word_reads_nat 15 64) nat_word_15_calculated

    theorem nat_word_16_calculated : natWord 16 64 = 0x4 :=
      (natWord_step 16 63 0x2 false
        (natWord_step 16 62 0x1 false
        (natWord_step 16 61 0x0 true
        (natWord_step 16 60 0x0 false
        (natWord_step 16 59 0x0 false
        (natWord_step 16 58 0x0 false
        (natWord_step 16 57 0x0 false
        (natWord_step 16 56 0x0 false
        (natWord_step 16 55 0x0 false
        (natWord_step 16 54 0x0 false
        (natWord_step 16 53 0x0 false
        (natWord_step 16 52 0x0 false
        (natWord_step 16 51 0x0 false
        (natWord_step 16 50 0x0 false
        (natWord_step 16 49 0x0 false
        (natWord_step 16 48 0x0 false
        (natWord_step 16 47 0x0 false
        (natWord_step 16 46 0x0 false
        (natWord_step 16 45 0x0 false
        (natWord_step 16 44 0x0 false
        (natWord_step 16 43 0x0 false
        (natWord_step 16 42 0x0 false
        (natWord_step 16 41 0x0 false
        (natWord_step 16 40 0x0 false
        (natWord_step 16 39 0x0 false
        (natWord_step 16 38 0x0 false
        (natWord_step 16 37 0x0 false
        (natWord_step 16 36 0x0 false
        (natWord_step 16 35 0x0 false
        (natWord_step 16 34 0x0 false
        (natWord_step 16 33 0x0 false
        (natWord_step 16 32 0x0 false
        (natWord_step 16 31 0x0 false
        (natWord_step 16 30 0x0 false
        (natWord_step 16 29 0x0 false
        (natWord_step 16 28 0x0 false
        (natWord_step 16 27 0x0 false
        (natWord_step 16 26 0x0 false
        (natWord_step 16 25 0x0 false
        (natWord_step 16 24 0x0 false
        (natWord_step 16 23 0x0 false
        (natWord_step 16 22 0x0 false
        (natWord_step 16 21 0x0 false
        (natWord_step 16 20 0x0 false
        (natWord_step 16 19 0x0 false
        (natWord_step 16 18 0x0 false
        (natWord_step 16 17 0x0 false
        (natWord_step 16 16 0x0 false
        (natWord_step 16 15 0x0 false
        (natWord_step 16 14 0x0 false
        (natWord_step 16 13 0x0 false
        (natWord_step 16 12 0x0 false
        (natWord_step 16 11 0x0 false
        (natWord_step 16 10 0x0 false
        (natWord_step 16 9 0x0 false
        (natWord_step 16 8 0x0 false
        (natWord_step 16 7 0x0 false
        (natWord_step 16 6 0x0 false
        (natWord_step 16 5 0x0 false
        (natWord_step 16 4 0x0 false
        (natWord_step 16 3 0x0 false
        (natWord_step 16 2 0x0 false
        (natWord_step 16 1 0x0 false
        (natWord_step 16 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 1024 = false))
        (by decide : natBit 1025 = false))
        (by decide : natBit 1026 = false))
        (by decide : natBit 1027 = false))
        (by decide : natBit 1028 = false))
        (by decide : natBit 1029 = false))
        (by decide : natBit 1030 = false))
        (by decide : natBit 1031 = false))
        (by decide : natBit 1032 = false))
        (by decide : natBit 1033 = false))
        (by decide : natBit 1034 = false))
        (by decide : natBit 1035 = false))
        (by decide : natBit 1036 = false))
        (by decide : natBit 1037 = false))
        (by decide : natBit 1038 = false))
        (by decide : natBit 1039 = false))
        (by decide : natBit 1040 = false))
        (by decide : natBit 1041 = false))
        (by decide : natBit 1042 = false))
        (by decide : natBit 1043 = false))
        (by decide : natBit 1044 = false))
        (by decide : natBit 1045 = false))
        (by decide : natBit 1046 = false))
        (by decide : natBit 1047 = false))
        (by decide : natBit 1048 = false))
        (by decide : natBit 1049 = false))
        (by decide : natBit 1050 = false))
        (by decide : natBit 1051 = false))
        (by decide : natBit 1052 = false))
        (by decide : natBit 1053 = false))
        (by decide : natBit 1054 = false))
        (by decide : natBit 1055 = false))
        (by decide : natBit 1056 = false))
        (by decide : natBit 1057 = false))
        (by decide : natBit 1058 = false))
        (by decide : natBit 1059 = false))
        (by decide : natBit 1060 = false))
        (by decide : natBit 1061 = false))
        (by decide : natBit 1062 = false))
        (by decide : natBit 1063 = false))
        (by decide : natBit 1064 = false))
        (by decide : natBit 1065 = false))
        (by decide : natBit 1066 = false))
        (by decide : natBit 1067 = false))
        (by decide : natBit 1068 = false))
        (by decide : natBit 1069 = false))
        (by decide : natBit 1070 = false))
        (by decide : natBit 1071 = false))
        (by decide : natBit 1072 = false))
        (by decide : natBit 1073 = false))
        (by decide : natBit 1074 = false))
        (by decide : natBit 1075 = false))
        (by decide : natBit 1076 = false))
        (by decide : natBit 1077 = false))
        (by decide : natBit 1078 = false))
        (by decide : natBit 1079 = false))
        (by decide : natBit 1080 = false))
        (by decide : natBit 1081 = false))
        (by decide : natBit 1082 = false))
        (by decide : natBit 1083 = false))
        (by decide : natBit 1084 = false))
        (by decide : natBit 1085 = true))
        (by decide : natBit 1086 = false))
        (by decide : natBit 1087 = false))

    theorem nat_word_16 : wordWith room_as_nat 16 64 = 0x4 :=
      Eq.trans (word_reads_nat 16 64) nat_word_16_calculated

    theorem nat_word_17_calculated : natWord 17 64 = 0x0 :=
      (natWord_step 17 63 0x0 false
        (natWord_step 17 62 0x0 false
        (natWord_step 17 61 0x0 false
        (natWord_step 17 60 0x0 false
        (natWord_step 17 59 0x0 false
        (natWord_step 17 58 0x0 false
        (natWord_step 17 57 0x0 false
        (natWord_step 17 56 0x0 false
        (natWord_step 17 55 0x0 false
        (natWord_step 17 54 0x0 false
        (natWord_step 17 53 0x0 false
        (natWord_step 17 52 0x0 false
        (natWord_step 17 51 0x0 false
        (natWord_step 17 50 0x0 false
        (natWord_step 17 49 0x0 false
        (natWord_step 17 48 0x0 false
        (natWord_step 17 47 0x0 false
        (natWord_step 17 46 0x0 false
        (natWord_step 17 45 0x0 false
        (natWord_step 17 44 0x0 false
        (natWord_step 17 43 0x0 false
        (natWord_step 17 42 0x0 false
        (natWord_step 17 41 0x0 false
        (natWord_step 17 40 0x0 false
        (natWord_step 17 39 0x0 false
        (natWord_step 17 38 0x0 false
        (natWord_step 17 37 0x0 false
        (natWord_step 17 36 0x0 false
        (natWord_step 17 35 0x0 false
        (natWord_step 17 34 0x0 false
        (natWord_step 17 33 0x0 false
        (natWord_step 17 32 0x0 false
        (natWord_step 17 31 0x0 false
        (natWord_step 17 30 0x0 false
        (natWord_step 17 29 0x0 false
        (natWord_step 17 28 0x0 false
        (natWord_step 17 27 0x0 false
        (natWord_step 17 26 0x0 false
        (natWord_step 17 25 0x0 false
        (natWord_step 17 24 0x0 false
        (natWord_step 17 23 0x0 false
        (natWord_step 17 22 0x0 false
        (natWord_step 17 21 0x0 false
        (natWord_step 17 20 0x0 false
        (natWord_step 17 19 0x0 false
        (natWord_step 17 18 0x0 false
        (natWord_step 17 17 0x0 false
        (natWord_step 17 16 0x0 false
        (natWord_step 17 15 0x0 false
        (natWord_step 17 14 0x0 false
        (natWord_step 17 13 0x0 false
        (natWord_step 17 12 0x0 false
        (natWord_step 17 11 0x0 false
        (natWord_step 17 10 0x0 false
        (natWord_step 17 9 0x0 false
        (natWord_step 17 8 0x0 false
        (natWord_step 17 7 0x0 false
        (natWord_step 17 6 0x0 false
        (natWord_step 17 5 0x0 false
        (natWord_step 17 4 0x0 false
        (natWord_step 17 3 0x0 false
        (natWord_step 17 2 0x0 false
        (natWord_step 17 1 0x0 false
        (natWord_step 17 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 1088 = false))
        (by decide : natBit 1089 = false))
        (by decide : natBit 1090 = false))
        (by decide : natBit 1091 = false))
        (by decide : natBit 1092 = false))
        (by decide : natBit 1093 = false))
        (by decide : natBit 1094 = false))
        (by decide : natBit 1095 = false))
        (by decide : natBit 1096 = false))
        (by decide : natBit 1097 = false))
        (by decide : natBit 1098 = false))
        (by decide : natBit 1099 = false))
        (by decide : natBit 1100 = false))
        (by decide : natBit 1101 = false))
        (by decide : natBit 1102 = false))
        (by decide : natBit 1103 = false))
        (by decide : natBit 1104 = false))
        (by decide : natBit 1105 = false))
        (by decide : natBit 1106 = false))
        (by decide : natBit 1107 = false))
        (by decide : natBit 1108 = false))
        (by decide : natBit 1109 = false))
        (by decide : natBit 1110 = false))
        (by decide : natBit 1111 = false))
        (by decide : natBit 1112 = false))
        (by decide : natBit 1113 = false))
        (by decide : natBit 1114 = false))
        (by decide : natBit 1115 = false))
        (by decide : natBit 1116 = false))
        (by decide : natBit 1117 = false))
        (by decide : natBit 1118 = false))
        (by decide : natBit 1119 = false))
        (by decide : natBit 1120 = false))
        (by decide : natBit 1121 = false))
        (by decide : natBit 1122 = false))
        (by decide : natBit 1123 = false))
        (by decide : natBit 1124 = false))
        (by decide : natBit 1125 = false))
        (by decide : natBit 1126 = false))
        (by decide : natBit 1127 = false))
        (by decide : natBit 1128 = false))
        (by decide : natBit 1129 = false))
        (by decide : natBit 1130 = false))
        (by decide : natBit 1131 = false))
        (by decide : natBit 1132 = false))
        (by decide : natBit 1133 = false))
        (by decide : natBit 1134 = false))
        (by decide : natBit 1135 = false))
        (by decide : natBit 1136 = false))
        (by decide : natBit 1137 = false))
        (by decide : natBit 1138 = false))
        (by decide : natBit 1139 = false))
        (by decide : natBit 1140 = false))
        (by decide : natBit 1141 = false))
        (by decide : natBit 1142 = false))
        (by decide : natBit 1143 = false))
        (by decide : natBit 1144 = false))
        (by decide : natBit 1145 = false))
        (by decide : natBit 1146 = false))
        (by decide : natBit 1147 = false))
        (by decide : natBit 1148 = false))
        (by decide : natBit 1149 = false))
        (by decide : natBit 1150 = false))
        (by decide : natBit 1151 = false))

    theorem nat_word_17 : wordWith room_as_nat 17 64 = 0x0 :=
      Eq.trans (word_reads_nat 17 64) nat_word_17_calculated

    theorem nat_word_18_calculated : natWord 18 64 = 0x0 :=
      (natWord_step 18 63 0x0 false
        (natWord_step 18 62 0x0 false
        (natWord_step 18 61 0x0 false
        (natWord_step 18 60 0x0 false
        (natWord_step 18 59 0x0 false
        (natWord_step 18 58 0x0 false
        (natWord_step 18 57 0x0 false
        (natWord_step 18 56 0x0 false
        (natWord_step 18 55 0x0 false
        (natWord_step 18 54 0x0 false
        (natWord_step 18 53 0x0 false
        (natWord_step 18 52 0x0 false
        (natWord_step 18 51 0x0 false
        (natWord_step 18 50 0x0 false
        (natWord_step 18 49 0x0 false
        (natWord_step 18 48 0x0 false
        (natWord_step 18 47 0x0 false
        (natWord_step 18 46 0x0 false
        (natWord_step 18 45 0x0 false
        (natWord_step 18 44 0x0 false
        (natWord_step 18 43 0x0 false
        (natWord_step 18 42 0x0 false
        (natWord_step 18 41 0x0 false
        (natWord_step 18 40 0x0 false
        (natWord_step 18 39 0x0 false
        (natWord_step 18 38 0x0 false
        (natWord_step 18 37 0x0 false
        (natWord_step 18 36 0x0 false
        (natWord_step 18 35 0x0 false
        (natWord_step 18 34 0x0 false
        (natWord_step 18 33 0x0 false
        (natWord_step 18 32 0x0 false
        (natWord_step 18 31 0x0 false
        (natWord_step 18 30 0x0 false
        (natWord_step 18 29 0x0 false
        (natWord_step 18 28 0x0 false
        (natWord_step 18 27 0x0 false
        (natWord_step 18 26 0x0 false
        (natWord_step 18 25 0x0 false
        (natWord_step 18 24 0x0 false
        (natWord_step 18 23 0x0 false
        (natWord_step 18 22 0x0 false
        (natWord_step 18 21 0x0 false
        (natWord_step 18 20 0x0 false
        (natWord_step 18 19 0x0 false
        (natWord_step 18 18 0x0 false
        (natWord_step 18 17 0x0 false
        (natWord_step 18 16 0x0 false
        (natWord_step 18 15 0x0 false
        (natWord_step 18 14 0x0 false
        (natWord_step 18 13 0x0 false
        (natWord_step 18 12 0x0 false
        (natWord_step 18 11 0x0 false
        (natWord_step 18 10 0x0 false
        (natWord_step 18 9 0x0 false
        (natWord_step 18 8 0x0 false
        (natWord_step 18 7 0x0 false
        (natWord_step 18 6 0x0 false
        (natWord_step 18 5 0x0 false
        (natWord_step 18 4 0x0 false
        (natWord_step 18 3 0x0 false
        (natWord_step 18 2 0x0 false
        (natWord_step 18 1 0x0 false
        (natWord_step 18 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 1152 = false))
        (by decide : natBit 1153 = false))
        (by decide : natBit 1154 = false))
        (by decide : natBit 1155 = false))
        (by decide : natBit 1156 = false))
        (by decide : natBit 1157 = false))
        (by decide : natBit 1158 = false))
        (by decide : natBit 1159 = false))
        (by decide : natBit 1160 = false))
        (by decide : natBit 1161 = false))
        (by decide : natBit 1162 = false))
        (by decide : natBit 1163 = false))
        (by decide : natBit 1164 = false))
        (by decide : natBit 1165 = false))
        (by decide : natBit 1166 = false))
        (by decide : natBit 1167 = false))
        (by decide : natBit 1168 = false))
        (by decide : natBit 1169 = false))
        (by decide : natBit 1170 = false))
        (by decide : natBit 1171 = false))
        (by decide : natBit 1172 = false))
        (by decide : natBit 1173 = false))
        (by decide : natBit 1174 = false))
        (by decide : natBit 1175 = false))
        (by decide : natBit 1176 = false))
        (by decide : natBit 1177 = false))
        (by decide : natBit 1178 = false))
        (by decide : natBit 1179 = false))
        (by decide : natBit 1180 = false))
        (by decide : natBit 1181 = false))
        (by decide : natBit 1182 = false))
        (by decide : natBit 1183 = false))
        (by decide : natBit 1184 = false))
        (by decide : natBit 1185 = false))
        (by decide : natBit 1186 = false))
        (by decide : natBit 1187 = false))
        (by decide : natBit 1188 = false))
        (by decide : natBit 1189 = false))
        (by decide : natBit 1190 = false))
        (by decide : natBit 1191 = false))
        (by decide : natBit 1192 = false))
        (by decide : natBit 1193 = false))
        (by decide : natBit 1194 = false))
        (by decide : natBit 1195 = false))
        (by decide : natBit 1196 = false))
        (by decide : natBit 1197 = false))
        (by decide : natBit 1198 = false))
        (by decide : natBit 1199 = false))
        (by decide : natBit 1200 = false))
        (by decide : natBit 1201 = false))
        (by decide : natBit 1202 = false))
        (by decide : natBit 1203 = false))
        (by decide : natBit 1204 = false))
        (by decide : natBit 1205 = false))
        (by decide : natBit 1206 = false))
        (by decide : natBit 1207 = false))
        (by decide : natBit 1208 = false))
        (by decide : natBit 1209 = false))
        (by decide : natBit 1210 = false))
        (by decide : natBit 1211 = false))
        (by decide : natBit 1212 = false))
        (by decide : natBit 1213 = false))
        (by decide : natBit 1214 = false))
        (by decide : natBit 1215 = false))

    theorem nat_word_18 : wordWith room_as_nat 18 64 = 0x0 :=
      Eq.trans (word_reads_nat 18 64) nat_word_18_calculated

    theorem nat_word_19_calculated : natWord 19 64 = 0x0 :=
      (natWord_step 19 63 0x0 false
        (natWord_step 19 62 0x0 false
        (natWord_step 19 61 0x0 false
        (natWord_step 19 60 0x0 false
        (natWord_step 19 59 0x0 false
        (natWord_step 19 58 0x0 false
        (natWord_step 19 57 0x0 false
        (natWord_step 19 56 0x0 false
        (natWord_step 19 55 0x0 false
        (natWord_step 19 54 0x0 false
        (natWord_step 19 53 0x0 false
        (natWord_step 19 52 0x0 false
        (natWord_step 19 51 0x0 false
        (natWord_step 19 50 0x0 false
        (natWord_step 19 49 0x0 false
        (natWord_step 19 48 0x0 false
        (natWord_step 19 47 0x0 false
        (natWord_step 19 46 0x0 false
        (natWord_step 19 45 0x0 false
        (natWord_step 19 44 0x0 false
        (natWord_step 19 43 0x0 false
        (natWord_step 19 42 0x0 false
        (natWord_step 19 41 0x0 false
        (natWord_step 19 40 0x0 false
        (natWord_step 19 39 0x0 false
        (natWord_step 19 38 0x0 false
        (natWord_step 19 37 0x0 false
        (natWord_step 19 36 0x0 false
        (natWord_step 19 35 0x0 false
        (natWord_step 19 34 0x0 false
        (natWord_step 19 33 0x0 false
        (natWord_step 19 32 0x0 false
        (natWord_step 19 31 0x0 false
        (natWord_step 19 30 0x0 false
        (natWord_step 19 29 0x0 false
        (natWord_step 19 28 0x0 false
        (natWord_step 19 27 0x0 false
        (natWord_step 19 26 0x0 false
        (natWord_step 19 25 0x0 false
        (natWord_step 19 24 0x0 false
        (natWord_step 19 23 0x0 false
        (natWord_step 19 22 0x0 false
        (natWord_step 19 21 0x0 false
        (natWord_step 19 20 0x0 false
        (natWord_step 19 19 0x0 false
        (natWord_step 19 18 0x0 false
        (natWord_step 19 17 0x0 false
        (natWord_step 19 16 0x0 false
        (natWord_step 19 15 0x0 false
        (natWord_step 19 14 0x0 false
        (natWord_step 19 13 0x0 false
        (natWord_step 19 12 0x0 false
        (natWord_step 19 11 0x0 false
        (natWord_step 19 10 0x0 false
        (natWord_step 19 9 0x0 false
        (natWord_step 19 8 0x0 false
        (natWord_step 19 7 0x0 false
        (natWord_step 19 6 0x0 false
        (natWord_step 19 5 0x0 false
        (natWord_step 19 4 0x0 false
        (natWord_step 19 3 0x0 false
        (natWord_step 19 2 0x0 false
        (natWord_step 19 1 0x0 false
        (natWord_step 19 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 1216 = false))
        (by decide : natBit 1217 = false))
        (by decide : natBit 1218 = false))
        (by decide : natBit 1219 = false))
        (by decide : natBit 1220 = false))
        (by decide : natBit 1221 = false))
        (by decide : natBit 1222 = false))
        (by decide : natBit 1223 = false))
        (by decide : natBit 1224 = false))
        (by decide : natBit 1225 = false))
        (by decide : natBit 1226 = false))
        (by decide : natBit 1227 = false))
        (by decide : natBit 1228 = false))
        (by decide : natBit 1229 = false))
        (by decide : natBit 1230 = false))
        (by decide : natBit 1231 = false))
        (by decide : natBit 1232 = false))
        (by decide : natBit 1233 = false))
        (by decide : natBit 1234 = false))
        (by decide : natBit 1235 = false))
        (by decide : natBit 1236 = false))
        (by decide : natBit 1237 = false))
        (by decide : natBit 1238 = false))
        (by decide : natBit 1239 = false))
        (by decide : natBit 1240 = false))
        (by decide : natBit 1241 = false))
        (by decide : natBit 1242 = false))
        (by decide : natBit 1243 = false))
        (by decide : natBit 1244 = false))
        (by decide : natBit 1245 = false))
        (by decide : natBit 1246 = false))
        (by decide : natBit 1247 = false))
        (by decide : natBit 1248 = false))
        (by decide : natBit 1249 = false))
        (by decide : natBit 1250 = false))
        (by decide : natBit 1251 = false))
        (by decide : natBit 1252 = false))
        (by decide : natBit 1253 = false))
        (by decide : natBit 1254 = false))
        (by decide : natBit 1255 = false))
        (by decide : natBit 1256 = false))
        (by decide : natBit 1257 = false))
        (by decide : natBit 1258 = false))
        (by decide : natBit 1259 = false))
        (by decide : natBit 1260 = false))
        (by decide : natBit 1261 = false))
        (by decide : natBit 1262 = false))
        (by decide : natBit 1263 = false))
        (by decide : natBit 1264 = false))
        (by decide : natBit 1265 = false))
        (by decide : natBit 1266 = false))
        (by decide : natBit 1267 = false))
        (by decide : natBit 1268 = false))
        (by decide : natBit 1269 = false))
        (by decide : natBit 1270 = false))
        (by decide : natBit 1271 = false))
        (by decide : natBit 1272 = false))
        (by decide : natBit 1273 = false))
        (by decide : natBit 1274 = false))
        (by decide : natBit 1275 = false))
        (by decide : natBit 1276 = false))
        (by decide : natBit 1277 = false))
        (by decide : natBit 1278 = false))
        (by decide : natBit 1279 = false))

    theorem nat_word_19 : wordWith room_as_nat 19 64 = 0x0 :=
      Eq.trans (word_reads_nat 19 64) nat_word_19_calculated

    theorem nat_word_20_calculated : natWord 20 64 = 0x810000000000 :=
      (natWord_step 20 63 0x408000000000 false
        (natWord_step 20 62 0x204000000000 false
        (natWord_step 20 61 0x102000000000 false
        (natWord_step 20 60 0x81000000000 false
        (natWord_step 20 59 0x40800000000 false
        (natWord_step 20 58 0x20400000000 false
        (natWord_step 20 57 0x10200000000 false
        (natWord_step 20 56 0x8100000000 false
        (natWord_step 20 55 0x4080000000 false
        (natWord_step 20 54 0x2040000000 false
        (natWord_step 20 53 0x1020000000 false
        (natWord_step 20 52 0x810000000 false
        (natWord_step 20 51 0x408000000 false
        (natWord_step 20 50 0x204000000 false
        (natWord_step 20 49 0x102000000 false
        (natWord_step 20 48 0x81000000 false
        (natWord_step 20 47 0x40800000 false
        (natWord_step 20 46 0x20400000 false
        (natWord_step 20 45 0x10200000 false
        (natWord_step 20 44 0x8100000 false
        (natWord_step 20 43 0x4080000 false
        (natWord_step 20 42 0x2040000 false
        (natWord_step 20 41 0x1020000 false
        (natWord_step 20 40 0x810000 false
        (natWord_step 20 39 0x408000 false
        (natWord_step 20 38 0x204000 false
        (natWord_step 20 37 0x102000 false
        (natWord_step 20 36 0x81000 false
        (natWord_step 20 35 0x40800 false
        (natWord_step 20 34 0x20400 false
        (natWord_step 20 33 0x10200 false
        (natWord_step 20 32 0x8100 false
        (natWord_step 20 31 0x4080 false
        (natWord_step 20 30 0x2040 false
        (natWord_step 20 29 0x1020 false
        (natWord_step 20 28 0x810 false
        (natWord_step 20 27 0x408 false
        (natWord_step 20 26 0x204 false
        (natWord_step 20 25 0x102 false
        (natWord_step 20 24 0x81 false
        (natWord_step 20 23 0x40 true
        (natWord_step 20 22 0x20 false
        (natWord_step 20 21 0x10 false
        (natWord_step 20 20 0x8 false
        (natWord_step 20 19 0x4 false
        (natWord_step 20 18 0x2 false
        (natWord_step 20 17 0x1 false
        (natWord_step 20 16 0x0 true
        (natWord_step 20 15 0x0 false
        (natWord_step 20 14 0x0 false
        (natWord_step 20 13 0x0 false
        (natWord_step 20 12 0x0 false
        (natWord_step 20 11 0x0 false
        (natWord_step 20 10 0x0 false
        (natWord_step 20 9 0x0 false
        (natWord_step 20 8 0x0 false
        (natWord_step 20 7 0x0 false
        (natWord_step 20 6 0x0 false
        (natWord_step 20 5 0x0 false
        (natWord_step 20 4 0x0 false
        (natWord_step 20 3 0x0 false
        (natWord_step 20 2 0x0 false
        (natWord_step 20 1 0x0 false
        (natWord_step 20 0 0x0 false
        (Eq.refl (0 : Nat))
        (by decide : natBit 1280 = false))
        (by decide : natBit 1281 = false))
        (by decide : natBit 1282 = false))
        (by decide : natBit 1283 = false))
        (by decide : natBit 1284 = false))
        (by decide : natBit 1285 = false))
        (by decide : natBit 1286 = false))
        (by decide : natBit 1287 = false))
        (by decide : natBit 1288 = false))
        (by decide : natBit 1289 = false))
        (by decide : natBit 1290 = false))
        (by decide : natBit 1291 = false))
        (by decide : natBit 1292 = false))
        (by decide : natBit 1293 = false))
        (by decide : natBit 1294 = false))
        (by decide : natBit 1295 = false))
        (by decide : natBit 1296 = true))
        (by decide : natBit 1297 = false))
        (by decide : natBit 1298 = false))
        (by decide : natBit 1299 = false))
        (by decide : natBit 1300 = false))
        (by decide : natBit 1301 = false))
        (by decide : natBit 1302 = false))
        (by decide : natBit 1303 = true))
        (by decide : natBit 1304 = false))
        (by decide : natBit 1305 = false))
        (by decide : natBit 1306 = false))
        (by decide : natBit 1307 = false))
        (by decide : natBit 1308 = false))
        (by decide : natBit 1309 = false))
        (by decide : natBit 1310 = false))
        (by decide : natBit 1311 = false))
        (by decide : natBit 1312 = false))
        (by decide : natBit 1313 = false))
        (by decide : natBit 1314 = false))
        (by decide : natBit 1315 = false))
        (by decide : natBit 1316 = false))
        (by decide : natBit 1317 = false))
        (by decide : natBit 1318 = false))
        (by decide : natBit 1319 = false))
        (by decide : natBit 1320 = false))
        (by decide : natBit 1321 = false))
        (by decide : natBit 1322 = false))
        (by decide : natBit 1323 = false))
        (by decide : natBit 1324 = false))
        (by decide : natBit 1325 = false))
        (by decide : natBit 1326 = false))
        (by decide : natBit 1327 = false))
        (by decide : natBit 1328 = false))
        (by decide : natBit 1329 = false))
        (by decide : natBit 1330 = false))
        (by decide : natBit 1331 = false))
        (by decide : natBit 1332 = false))
        (by decide : natBit 1333 = false))
        (by decide : natBit 1334 = false))
        (by decide : natBit 1335 = false))
        (by decide : natBit 1336 = false))
        (by decide : natBit 1337 = false))
        (by decide : natBit 1338 = false))
        (by decide : natBit 1339 = false))
        (by decide : natBit 1340 = false))
        (by decide : natBit 1341 = false))
        (by decide : natBit 1342 = false))
        (by decide : natBit 1343 = false))

    theorem nat_word_20 : wordWith room_as_nat 20 64 = 0x810000000000 :=
      Eq.trans (word_reads_nat 20 64) nat_word_20_calculated

    theorem nat_sheet_is : sheetWith room_as_nat 0 21 =
      [0xfdf3c70c17cf1c30, 0x403c70c30401c30c, 0x30400c1041000040, 0x7cf1c30403,
       0xc70c30401c30c304, 0xc30c3040041041, 0x3c70, 0xc30401c30c30400c, 0x30c30400c30c3040,
       0x410410000000000, 0x1c30c30400c30c, 0x30400c30c30400c3, 0xc30400410410000, 0xc104100,
       0x41041000041041, 0x410410000000, 0x4, 0x0, 0x0, 0x0, 0x810000000000] := by
      change [wordWith room_as_nat 0 64, wordWith room_as_nat 1 64, wordWith room_as_nat 2 64, wordWith room_as_nat 3 64, wordWith room_as_nat 4 64, wordWith room_as_nat 5 64, wordWith room_as_nat 6 64, wordWith room_as_nat 7 64, wordWith room_as_nat 8 64, wordWith room_as_nat 9 64, wordWith room_as_nat 10 64, wordWith room_as_nat 11 64, wordWith room_as_nat 12 64, wordWith room_as_nat 13 64, wordWith room_as_nat 14 64, wordWith room_as_nat 15 64, wordWith room_as_nat 16 64, wordWith room_as_nat 17 64, wordWith room_as_nat 18 64, wordWith room_as_nat 19 64, wordWith room_as_nat 20 64] = [0xfdf3c70c17cf1c30, 0x403c70c30401c30c, 0x30400c1041000040, 0x7cf1c30403, 0xc70c30401c30c304, 0xc30c3040041041, 0x3c70, 0xc30401c30c30400c, 0x30c30400c30c3040, 0x410410000000000, 0x1c30c30400c30c, 0x30400c30c30400c3, 0xc30400410410000, 0xc104100, 0x41041000041041, 0x410410000000, 0x4, 0x0, 0x0, 0x0, 0x810000000000]
      exact nat_sheet_cons nat_word_0 (nat_sheet_cons nat_word_1 (nat_sheet_cons nat_word_2 (nat_sheet_cons nat_word_3 (nat_sheet_cons nat_word_4 (nat_sheet_cons nat_word_5 (nat_sheet_cons nat_word_6 (nat_sheet_cons nat_word_7 (nat_sheet_cons nat_word_8 (nat_sheet_cons nat_word_9 (nat_sheet_cons nat_word_10 (nat_sheet_cons nat_word_11 (nat_sheet_cons nat_word_12 (nat_sheet_cons nat_word_13 (nat_sheet_cons nat_word_14 (nat_sheet_cons nat_word_15 (nat_sheet_cons nat_word_16 (nat_sheet_cons nat_word_17 (nat_sheet_cons nat_word_18 (nat_sheet_cons nat_word_19 (nat_sheet_cons nat_word_20 (Eq.refl ([] : List Nat))))))))))))))))))))))

    theorem sheet_is : sheet =
      [0xfdf3c70c17cf1c30, 0x403c70c30401c30c, 0x30400c1041000040, 0x7cf1c30403,
       0xc70c30401c30c304, 0xc30c3040041041, 0x3c70, 0xc30401c30c30400c, 0x30c30400c30c3040,
       0x410410000000000, 0x1c30c30400c30c, 0x30400c30c30400c3, 0xc30400410410000, 0xc104100,
       0x41041000041041, 0x410410000000, 0x4, 0x0, 0x0, 0x0, 0x810000000000] :=
      Eq.trans sheet_reads_nat nat_sheet_is

    end AtermMeshCapacity
}
