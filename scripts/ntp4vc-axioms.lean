import Lean

/-!
Read the compiled benchmark modules without importing Mathlib 600 times. Only declarations
owned by each original target and its supplied support modules enter its axiom policy.
No candidate or native-evaluation assertion is used to construct this list.
-/
open Lean

private structure Cache where
  axioms : Std.HashMap Name (Array Name) := {}
  regions : Array CompactedRegion := #[]

private def suppliedModule (name : Name) : Bool :=
  #["Why3.", "frama_c.", "pearl.", "NTPInputs."].any (name.toString.startsWith ·)

private partial def collect (module : Name) (cache : IO.Ref Cache) : IO (Array Name) := do
  if let some names := (← cache.get).axioms[module]? then return names
  let (data, region) ← readModuleData (← findOLean module)
  cache.modify fun s => { s with regions := s.regions.push region }
  let mut names : Std.HashSet Name := {}
  for constant in data.constants do
    if let .axiomInfo info := constant then names := names.insert info.name
  for dependency in data.imports do
    if suppliedModule dependency.module then
      for name in ← collect dependency.module cache do names := names.insert name
  let result := names.toArray
  cache.modify fun s => { s with axioms := s.axioms.insert module result }
  return result

def main (modules : List String) : IO Unit := do
  initSearchPath (← findSysroot)
  let cache ← IO.mkRef ({} : Cache)
  for module in modules do
    let names ← collect module.toName cache
    let names := (names.map Name.toString).qsort (· < ·)
    if names.contains "sorryAx" || names.any (fun n => (n.splitOn "_native.").length > 1) then
      throw <| IO.userError "An admission/native assertion is not a benchmark axiom"
    IO.println (Json.mkObj [("module", toJson module), ("permitted_axioms",
      toJson (#["propext", "Quot.sound", "Classical.choice"] ++ names))]).compress
