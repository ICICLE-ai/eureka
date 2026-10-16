//! Comparator as a `worktrees::Verifier`, run exactly as its README requires:
//! `landrun` for the build/export sandbox plus `systemd-run --user` with
//! `RestrictAddressFamilies=~AF_UNIX` (needed because this kernel's Landlock
//! cannot scope UNIX sockets).
//!
//! worktrees hands us a fresh copy of the trusted template with the candidate
//! as the only changed file, so nothing from the agent's workspace or build
//! outputs is used. Build Comparator first with `lake build lean4export
//! comparator` in its checkout.

use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use worktrees::{Verification, Verifier, VerifyError};

pub struct Comparator {
    pub comparator: PathBuf,
    pub lean4export: PathBuf,
    pub policy: String,
    pub timeout: Duration,
    /// Prebuilt `.lake/packages` linked into the fresh verification root.
    pub deps: Option<PathBuf>,
}

impl Comparator {
    /// Locate the binaries in a Comparator checkout and derive a policy ID from
    /// its git revision and the template's toolchain and config.
    pub fn from_checkout(dir: &Path, template: &Path) -> anyhow::Result<Self> {
        // Verification runs with another working directory; paths must be absolute.
        let dir = &std::fs::canonicalize(dir)?;
        let comparator = dir.join(".lake/build/bin/comparator");
        let lean4export = dir.join(".lake/packages/lean4export/.lake/build/bin/lean4export");
        for bin in [&comparator, &lean4export] {
            if !bin.is_file() {
                anyhow::bail!(
                    "missing {} (run `lake build lean4export comparator`)",
                    bin.display()
                );
            }
        }
        let rev = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|| "unknown".into());
        let toolchain =
            std::fs::read_to_string(template.join("lean-toolchain")).unwrap_or_default();
        let config = std::fs::read_to_string(template.join("config.json"))?;
        check_theorem_names(template, &config)?;
        let policy = format!(
            "comparator:{rev};toolchain:{};config:{}",
            toolchain.trim(),
            config.split_whitespace().collect::<String>()
        );
        Ok(Self {
            comparator,
            lean4export,
            policy,
            timeout: Duration::from_secs(1800),
            deps: None,
        })
    }
}

impl Verifier for Comparator {
    fn policy_id(&self) -> &str {
        &self.policy
    }

    fn verify(&self, input: &Verification<'_>) -> Result<String, VerifyError> {
        if let Some(deps) = &self.deps {
            crate::deps::link(input.root(), deps)
                .map_err(|e| VerifyError::Operational(std::io::Error::other(e.to_string())))?;
        }
        let path = std::env::var("PATH").unwrap_or_default();
        // Run as a transient unit so RuntimeMaxSec kills the whole process tree.
        // The unit doesn't inherit our environment, hence the explicit PATH.
        let output = Command::new("systemd-run")
            .args(["--user", "--wait", "--pipe", "--quiet", "--collect"])
            .arg("--property=RestrictAddressFamilies=~AF_UNIX")
            .arg(format!(
                "--property=RuntimeMaxSec={}",
                self.timeout.as_secs()
            ))
            .arg(format!("-EPATH={path}"))
            .arg(format!(
                "-ECOMPARATOR_LEAN4EXPORT={}",
                self.lean4export.display()
            ))
            .arg("--working-directory")
            .arg(input.root())
            .args(["--", "lake", "env"])
            .arg(&self.comparator)
            .arg("config.json")
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report = tail(&format!("{stdout}\n{stderr}"), 1024 * 1024 - 1024);
        classify(output.status.success(), &stdout, &stderr, report)
    }
}

// A nonzero exit alone doesn't mean the proof is wrong. Only output we recognize
// as a rejection counts as one. Anything else (a crash, a PANIC, setup trouble)
// is an operational failure, which search.rs retries once.
fn classify(
    success: bool,
    stdout: &str,
    stderr: &str,
    report: String,
) -> Result<String, VerifyError> {
    if success && stdout.contains("Your solution is okay!") {
        return Ok(report);
    }
    let text = format!("{stdout}\n{stderr}");
    let rejection = [
        "Illegal axiom detected",
        "does not match between challenge and target",
        "theorem statement do not match",
        "constant kind don't match",
        "Const not found in solution",
        "Constant not found in solution",
        "Solution constant is not a theorem",
        "Solution constant is not a definition",
        "Lean default kernel rejects the solution",
        "Nanoda kernel rejected the solution",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    let solution_build_error = stdout
        .lines()
        .rfind(|line| line.starts_with("Building ") || line.starts_with("Exporting "))
        .is_some_and(|line| line.starts_with("Building ") && line.contains("Solution"))
        && text.contains(".lean:")
        && text.contains("error:");
    if !text.contains("PANIC") && (rejection || solution_build_error) {
        Err(VerifyError::Rejected(report))
    } else {
        Err(VerifyError::Operational(std::io::Error::other(report)))
    }
}

/// Every name in `theorem_names` must be a theorem the challenge declares,
/// fully qualified. lean4export panics on a missing constant, after the agents
/// have done all their work, so check up front.
fn check_theorem_names(template: &Path, config: &str) -> anyhow::Result<()> {
    let config: serde_json::Value = serde_json::from_str(config)?;
    let module = config["challenge_module"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("config.json: challenge_module missing"))?;
    let path = template.join(module.replace('.', "/") + ".lean");
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("read challenge {}", path.display()))?;
    let declared = declared_theorems(&source);
    for name in config["theorem_names"].as_array().into_iter().flatten() {
        let name = name.as_str().unwrap_or_default();
        if !declared.iter().any(|d| d == name) {
            anyhow::bail!(
                "config.json names theorem `{name}`, but {} declares: {}",
                path.display(),
                declared.join(", ")
            );
        }
    }
    Ok(())
}

/// Fully qualified names of the theorems in a Lean source, following
/// `namespace`/`end` nesting.
/// A line scan, good enough to catch typos in config.json. It misses theorems
/// with anything in front of the keyword, such as `private theorem`.
fn declared_theorems(source: &str) -> Vec<String> {
    let mut namespaces: Vec<&str> = Vec::new();
    let mut names = Vec::new();
    for line in source.lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some("namespace"), Some(ns)) => namespaces.push(ns),
            (Some("end"), Some(ns)) if namespaces.last() == Some(&ns) => {
                namespaces.pop();
            }
            (Some("theorem" | "lemma"), Some(name)) => {
                let prefix = namespaces.join(".");
                names.push(if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}.{name}")
                });
            }
            _ => {}
        }
    }
    names
}

// worktrees caps reports at 1 MiB. Keep the end, which is where the verdict is.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.trim().to_owned();
    }
    let start = text.ceil_char_boundary(text.len() - max);
    text[start..].trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_require_proof_rejection_evidence() {
        assert!(matches!(
            classify(false, "", "Child exited with 139", "crash".into()),
            Err(VerifyError::Operational(_))
        ));
        assert!(matches!(
            classify(
                false,
                "Building Proofs.Solution\nerror: Proofs/Solution.lean:4:2: unsolved goals",
                "Child exited with 1",
                "build".into()
            ),
            Err(VerifyError::Rejected(_))
        ));
        assert!(matches!(
            classify(false, "", "Illegal axiom detected: sorryAx", "sorry".into()),
            Err(VerifyError::Rejected(_))
        ));
        assert!(matches!(
            classify(false, "", "Failed to connect to bus", "bus".into()),
            Err(VerifyError::Operational(_))
        ));
        assert!(classify(true, "Your solution is okay!", "", "ok".into()).is_ok());
        assert!(classify(true, "", "", "missing sentinel".into()).is_err());
    }

    #[test]
    fn theorems_are_qualified_by_namespace() {
        let src = "import Mathlib\nnamespace TriangleGame\ndef x := 1\ntheorem main_theorem : True := by\n  sorry\nend TriangleGame\nlemma top : True := trivial\n";
        assert_eq!(declared_theorems(src), ["TriangleGame.main_theorem", "top"]);
    }
}
