//! Text rules for Beam probes, kept pure so they can be tested on the exact
//! inputs models produce.

use std::borrow::Cow;

/// Keywords that begin a top-level command rather than a tactic.
const COMMANDS: &[&str] = &[
    "theorem",
    "lemma",
    "example",
    "def",
    "abbrev",
    "instance",
    "structure",
    "inductive",
    "class",
    "open",
    "namespace",
    "section",
    "end",
    "variable",
    "universe",
    "set_option",
    "noncomputable",
    "private",
    "protected",
    "macro",
    "syntax",
    "elab",
    "notation",
    "attribute",
    "import",
];

/// Beam's tactic mode accepts exactly one tactic node. A model usually writes a
/// sequence (`t1\n  t2` or `t1; t2`), which fails with "expected end of input"
/// at the second tactic. Wrapping the sequence in parentheses makes it one
/// node with the same meaning. Commands and already-grouped text are untouched.
pub fn tactic_block(text: &str) -> Cow<'_, str> {
    let dedented = dedent(text);
    let trimmed = dedented.trim();
    if trimmed.is_empty() || is_command(trimmed) || is_grouped(trimmed) {
        return if dedented == text {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(dedented)
        };
    }
    let multi_line = trimmed.lines().filter(|l| !l.trim().is_empty()).count() > 1;
    let sequenced = trimmed
        .lines()
        .next()
        .is_some_and(|l| l.replace("<;>", "").contains(';'));
    if !multi_line && !sequenced {
        return if dedented == text {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(dedented)
        };
    }
    let body = trimmed
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.to_owned()
            } else {
                format!(" {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Cow::Owned(format!("({body})"))
}

fn is_command(text: &str) -> bool {
    text.starts_with('#')
        || text.starts_with("@[")
        || text.starts_with("/-")
        || COMMANDS.iter().any(|k| {
            text.starts_with(k)
                && text[k.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
}

fn is_grouped(text: &str) -> bool {
    text.starts_with('(')
        || text.starts_with('·')
        || text.starts_with("first")
        || text.starts_with("by ")
        || text.starts_with("by\n")
}

/// Remove the indentation shared by all non-empty lines.
fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    if indent == 0 {
        return text.to_owned();
    }
    text.lines()
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The 0-based line at which a command-mode probe sees every declaration of
/// the file but the last: the start of the last top-level declaration. Lines
/// after it are inside that declaration, where Beam switches to tactic mode.
pub fn command_line(source: &str) -> usize {
    source
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            l.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '@')
                && !l.starts_with("import ")
        })
        .map(|(i, _)| i)
        .last()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tactics_and_commands_pass_through() {
        for text in [
            "omega",
            "exact Nat.add_comm a b",
            "simp [foo] at h ⊢",
            "#check @Nat.gcd_comm",
            "example (a : ℕ) : a = a := by\n  rfl",
            "theorem t : True := by\n  trivial",
            "first\n  | omega\n  | simp",
            "· rw [h]\n  rfl",
            "(rw [h]\n rfl)",
            "rw [Nat.add_comm] <;> rfl",
        ] {
            assert!(matches!(tactic_block(text), Cow::Borrowed(_)), "{text:?}");
        }
    }

    #[test]
    fn sequences_are_grouped() {
        assert_eq!(
            tactic_block("rw [h]; exact Nat.gcd_dvd_left a s.gcd"),
            "(rw [h]; exact Nat.gcd_dvd_left a s.gcd)"
        );
        assert_eq!(
            tactic_block("obtain ⟨m, n, s, hm⟩ := h\nsimp\nomega"),
            "(obtain ⟨m, n, s, hm⟩ := h\n simp\n omega)"
        );
        // Indented text from a model that copied the file's layout.
        assert_eq!(
            tactic_block(
                "    rcases Multiset.mem_cons.mp hb with h | hb\n    · simp [h]\n    · exact ih hb"
            ),
            "(rcases Multiset.mem_cons.mp hb with h | hb\n · simp [h]\n · exact ih hb)"
        );
        // A single indented tactic is only dedented.
        assert_eq!(tactic_block("  omega"), "omega");
        // Nested structure keeps its relative indentation.
        assert_eq!(
            tactic_block("by_cases hg : g = 1\n· have hx : 1 = 1 := rfl\n  omega\n· simp"),
            "(by_cases hg : g = 1\n · have hx : 1 = 1 := rfl\n   omega\n · simp)"
        );
    }

    #[test]
    fn command_line_is_the_last_declaration() {
        let src = "import Mathlib\n\n/-- doc -/\nabbrev Board := Multiset ℕ\n\ndef IsInitial (B : Board) : Prop :=\n  B.card = 2026\n\ntheorem t : True := by\n  trivial\n";
        assert_eq!(command_line(src), 8);
        assert_eq!(command_line("import Mathlib\n"), 0);
    }
}
