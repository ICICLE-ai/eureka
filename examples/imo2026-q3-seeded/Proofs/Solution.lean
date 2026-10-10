import Mathlib
set_option backward.isDefEq.respectTransparency false

open scoped BigOperators

namespace LiuBangXiangYu

/-- The multiset of piece lengths obtained by cutting `[0,1]` at the points of a
finite set `S ⊆ (0,1)`.  We sort `S` ascending, prepend `0` and append `1`, and
take consecutive differences.  The result is a list of `|S| + 1` positive reals
summing to `1` (when `S ⊆ (0,1)`). -/
noncomputable def pieceLengths (S : Finset ℝ) : List ℝ :=
  let l : List ℝ := (0 : ℝ) :: (S.sort (· ≤ ·)) ++ [1]
  List.zipWith (fun a b => b - a) l l.tail

/-- The sum of the entries of a list `L` at the (0-indexed) even positions, after
sorting `L` in non-increasing order.  These are the entries in the `1`st, `3`rd,
`5`th, … positions of the sorted (decreasing) list, i.e. the pieces claimed by
the first mover under the greedy claiming rule. -/
noncomputable def firstPlayerShare (L : List ℝ) : ℝ :=
  let sorted := L.mergeSort (· ≥ ·)
  ((sorted.zipIdx.filter (fun p => p.2 % 2 = 0)).map (fun p => p.1)).sum

/-- `L(A,B)`: Liu Bang's total length, given Liu Bang's marks `A` and Xiang Yu's
marks `B`. -/
noncomputable def L (A B : Finset ℝ) : ℝ :=
  firstPlayerShare (pieceLengths (A ∪ B))

/-- The set of admissible markings for a player: a finite subset of `(0,1)` of
size at most `n`.  We encode it as a `Finset ℝ` subject to the side conditions. -/
def AdmissibleMark (n : ℕ) (X : Finset ℝ) : Prop :=
  (↑X ⊆ Set.Ioo (0 : ℝ) 1) ∧ X.card ≤ n

/-- The value Liu Bang can guarantee.

`V n` is the supremum over Liu Bang's admissible markings `A` of the infimum,
over Xiang Yu's admissible markings `B` disjoint from `A`, of `L A B`. -/
noncomputable def V (n : ℕ) : ℝ :=
  ⨆ A : {A : Finset ℝ // AdmissibleMark n A},
    ⨅ B : {B : Finset ℝ // AdmissibleMark n B ∧ Disjoint A.1 B}, L A.1 B.1

/-- The claimed answer value `V(n) = 2^n / (2^(n+1) - 1)`. -/
noncomputable def answer (n : ℕ) : ℝ := (2 : ℝ) ^ n / ((2 : ℝ) ^ (n + 1) - 1)

/-! ## Auxiliary lemmas about the combinatorial layer -/

/-- A convenient normal form for `pieceLengths`: the list of consecutive
differences of `0 :: (S.sort (· ≤ ·)) ++ [1]`. -/
private lemma pieceLengths_eq_tail (S : Finset ℝ) :
    pieceLengths S =
      List.zipWith (fun a b : ℝ => b - a)
        ((0 : ℝ) :: (S.sort (· ≤ ·) ++ [1])) ((0 : ℝ) :: (S.sort (· ≤ ·) ++ [1])).tail := by
  simp only [pieceLengths, List.cons_append, List.tail_cons]

/-- `List.zipWith` of a list with its own tail is one element shorter. -/
private lemma zipWith_tail_length (f : ℝ → ℝ → ℝ) (l : List ℝ) :
    (List.zipWith f l l.tail).length = l.length - 1 := by
  rw [List.length_zipWith, List.length_tail]
  exact Nat.min_eq_right (Nat.sub_le _ _)

/-- Telescoping sums of consecutive differences. -/
private lemma sum_zipWith_sub_tail (u : List ℝ) (x y : ℝ) :
    (List.zipWith (fun a b : ℝ => b - a) (x :: (u ++ [y])) (u ++ [y])).sum = y - x := by
  induction u generalizing x with
  | nil => simp
  | cons a u ih =>
    simp only [List.cons_append, List.zipWith_cons_cons, List.sum_cons, ih a]
    ring

/-- Consecutive differences of a nondecreasing list of reals are nonnegative. -/
private lemma zipWith_tail_nonneg : ∀ (l : List ℝ), l.Pairwise (· ≤ ·) →
    ∀ x ∈ List.zipWith (fun a b : ℝ => b - a) l l.tail, 0 ≤ x
  | [], _ => by simp
  | a :: [], _ => by simp
  | a :: b :: rest, h => by
      obtain ⟨hab, hrest⟩ := List.pairwise_cons.mp h
      have hab' : a ≤ b := hab b (by simp)
      intro x hx
      rw [List.tail_cons, List.zipWith_cons_cons, List.mem_cons] at hx
      rcases hx with hx | hx
      · rw [hx]
        linarith
      · exact zipWith_tail_nonneg (b :: rest) hrest x hx

/-- All the piece lengths obtained from an admissible cut set are nonnegative. -/
private lemma pieceLengths_nonneg (S : Finset ℝ) (hS : ↑S ⊆ Set.Ioo (0 : ℝ) 1) :
    ∀ x ∈ pieceLengths S, 0 ≤ x := by
  have ht_nonneg : ∀ x ∈ S.sort (· ≤ ·), 0 ≤ x := by
    intro x hx
    have hxS : x ∈ S := by simpa using (Finset.mem_sort (· ≤ ·)).mp hx
    exact le_of_lt (hS hxS).1
  have ht_le_one : ∀ x ∈ S.sort (· ≤ ·), x ≤ 1 := by
    intro x hx
    have hxS : x ∈ S := by simpa using (Finset.mem_sort (· ≤ ·)).mp hx
    exact le_of_lt (hS hxS).2
  have h1 : (S.sort (· ≤ ·) ++ [1]).Pairwise (· ≤ ·) := by
    rw [List.pairwise_append]
    refine ⟨Finset.pairwise_sort S (· ≤ ·), by simp, ?_⟩
    intro a ha b hb
    simp only [List.mem_singleton] at hb
    rw [hb]
    exact ht_le_one a ha
  have h2 : ((0 : ℝ) :: (S.sort (· ≤ ·) ++ [1])).Pairwise (· ≤ ·) := by
    rw [List.pairwise_cons]
    refine ⟨?_, h1⟩
    intro a ha
    rcases List.mem_append.mp ha with h | h
    · exact ht_nonneg a h
    · simp only [List.mem_singleton] at h
      rw [h]
      norm_num
  intro x hx
  rw [pieceLengths_eq_tail] at hx
  exact zipWith_tail_nonneg _ h2 x hx

/-- `firstPlayerShare` of a list of nonnegative reals summing to `1` lies in
`[0,1]`: it is the sum of a sub-multiset of the entries. -/
private lemma firstPlayerShare_mem_Icc (t : List ℝ) (hnn : ∀ x ∈ t, 0 ≤ x)
    (hsum : t.sum = 1) : firstPlayerShare t ∈ Set.Icc (0 : ℝ) 1 := by
  set sorted := t.mergeSort (fun a b : ℝ => decide (a ≥ b)) with hsorted
  set sel := ((sorted.zipIdx).filter (fun p : ℝ × ℕ => p.2 % 2 = 0)).map
    (fun p : ℝ × ℕ => p.1) with hsel
  have heq : firstPlayerShare t = sel.sum := by
    dsimp only [firstPlayerShare]
  have hperm : sorted.Perm t := by
    rw [hsorted]
    exact List.mergeSort_perm t _
  have hnn' : ∀ x ∈ sorted, 0 ≤ x := fun x hx => hnn x ((hperm.mem_iff).mp hx)
  have hsub : sel.Sublist sorted := by
    rw [hsel]
    have h1 : ((sorted.zipIdx).filter (fun p : ℝ × ℕ => p.2 % 2 = 0)).Sublist
        sorted.zipIdx := List.filter_sublist
    have h2 : (((sorted.zipIdx).filter (fun p : ℝ × ℕ => p.2 % 2 = 0)).map
        (fun p : ℝ × ℕ => p.1)).Sublist ((sorted.zipIdx).map (fun p : ℝ × ℕ => p.1)) :=
      h1.map _
    rwa [List.zipIdx_map_fst 0 sorted] at h2
  rw [heq, Set.mem_Icc]
  exact ⟨List.sum_nonneg (fun x hx => hnn' x (hsub.subset hx)),
    (hsub.sum_le_sum hnn').trans (le_of_eq hperm.sum_eq) |>.trans (le_of_eq hsum)⟩

/-! ## Correctness statements for the definitions

These pin down that the encoded definitions behave as intended. -/

/-- The piece lengths of an admissible cut set sum to `1` (the total stick
length). -/
theorem pieceLengths_sum (S : Finset ℝ) (hS : ↑S ⊆ Set.Ioo (0 : ℝ) 1) :
    (pieceLengths S).sum = 1 := by
  rw [pieceLengths_eq_tail, List.tail_cons]
  simpa using sum_zipWith_sub_tail (S.sort (· ≤ ·)) 0 1

/-- There are `|S| + 1` pieces. -/
theorem pieceLengths_length (S : Finset ℝ) :
    (pieceLengths S).length = S.card + 1 := by
  rw [pieceLengths_eq_tail, zipWith_tail_length]
  simp [Finset.length_sort]

/-- Basic sanity bound: Liu Bang's share lies in `[0, 1]` for admissible cut
sets (it is a subset-sum of the piece lengths, which are nonnegative and sum to
`1`). -/
theorem L_mem_Icc (A B : Finset ℝ)
    (hA : ↑A ⊆ Set.Ioo (0 : ℝ) 1) (hB : ↑B ⊆ Set.Ioo (0 : ℝ) 1) :
    L A B ∈ Set.Icc (0 : ℝ) 1 := by
  have hAB : (↑(A ∪ B) : Set ℝ) ⊆ Set.Ioo (0 : ℝ) 1 := by
    intro x hx
    rw [Finset.mem_coe, Finset.mem_union] at hx
    rcases hx with hx | hx
    · exact hA hx
    · exact hB hx
  exact firstPlayerShare_mem_Icc (pieceLengths (A ∪ B)) (pieceLengths_nonneg (A ∪ B) hAB)
    (pieceLengths_sum (A ∪ B) hAB)

/-! ## Main Statements -/

/-- **Main statement.** For every positive integer `n`, Liu Bang's guaranteed
value equals `2^n / (2^(n+1) - 1)`. -/
theorem V_eq (n : ℕ) (hn : 0 < n) : V n = (2 : ℝ) ^ n / ((2 : ℝ) ^ (n + 1) - 1) := by
  sorry

/-- **Lower bound.** Liu Bang has an admissible marking `A` such that for every
admissible marking `B` disjoint from `A`, his guaranteed share is at least
`2^n / (2^(n+1) - 1)`. -/
theorem lower_bound (n : ℕ) (hn : 0 < n) :
    ∃ A : Finset ℝ, AdmissibleMark n A ∧
      ∀ B : Finset ℝ, AdmissibleMark n B → Disjoint A B →
        (2 : ℝ) ^ n / ((2 : ℝ) ^ (n + 1) - 1) ≤ L A B := by
  sorry

/-- **Upper bound / optimality.** For every admissible marking `A` of Liu Bang,
Xiang Yu has an admissible marking `B` disjoint from `A` with
`L A B ≤ 2^n / (2^(n+1) - 1)`, so Liu Bang cannot guarantee more. -/
theorem upper_bound (n : ℕ) (hn : 0 < n) :
    ∀ A : Finset ℝ, AdmissibleMark n A →
      ∃ B : Finset ℝ, AdmissibleMark n B ∧ Disjoint A B ∧
        L A B ≤ (2 : ℝ) ^ n / ((2 : ℝ) ^ (n + 1) - 1) := by
  sorry

end LiuBangXiangYu
