//! The Beam service: a gateway and elastic workers built from Beam's pool
//! image (`deploy/beam-pool` in the lean-beam checkout). Sandboxes attach to it
//! for speculative execution; sync, goals and todo stay on the sandbox's own
//! Lean server. This module knows the image's layout and the attach protocol.
//!
//! The pool must already be running (see `harness/deploy/README.md`). Beam's
//! side of the attach protocol is `lean-beam/scripts/beam_pool`.

use crate::sandbox::{Sandbox, WORK};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Paths inside Beam's pool image.
pub mod image {
    pub const BEAM_HOME: &str = "/opt/beam";
    pub const MCP: &str = "/opt/beam/.lake/build/bin/lean-beam-mcp";
    pub const PLUGIN: &str = "/opt/beam/.lake/build/lib/libbeam_Beam_LSP.so";
    pub const LEAN: &str = "/opt/lean/bin/lean";
    /// The prepared project the workers were built from.
    pub const PROJECT: &str = "/project";
}

/// Files of the prepared project that attachment compares against the
/// workers' snapshot. Everything else in an attempt root is a source edit that
/// Beam syncs after attachment.
const PREPARED: &[&str] = &[
    "lakefile.toml",
    "lake-manifest.json",
    "lean-toolchain",
    ".lake",
    ".beam",
];
const SESSION_DIR: &str = ".beam";
// Under .beam, which main.rs excludes from snapshots.
const STAGED: &str = ".beam/staged";

#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// `host:port` of the gateway as seen from the agents network.
    pub endpoint: String,
    /// Shared secret from `--pool-token-file`.
    pub token: String,
}

impl PoolConfig {
    /// Environment every sandbox command needs to use Beam against the pool.
    pub fn env(&self) -> Vec<(String, String)> {
        // Paths inside the container.
        [
            ("BEAM_HOME", image::BEAM_HOME.to_owned()),
            ("BEAM_SESSION_ROOT", format!("{WORK}/{SESSION_DIR}")),
            (
                "BEAM_LEAN_POOL_CONFIG",
                format!("{WORK}/{SESSION_DIR}/pool.json"),
            ),
            ("BEAM_POOL_TOKEN", self.token.clone()),
            ("PYTHONPATH", image::BEAM_HOME.to_owned()),
            (
                "PATH",
                "/opt/lean/bin:/usr/local/bin:/usr/bin:/bin".to_owned(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    /// Attach the attempt in `sandbox` (whose root is `root` on the host) to
    /// the pool. Attachment requires the root to match the workers' prepared
    /// snapshot exactly, so the problem files are staged aside under `.beam`
    /// (excluded from the snapshot) for the duration of the call.
    pub async fn attach(&self, sandbox: &Sandbox, root: &Path) -> Result<()> {
        let staged = root.join(STAGED);
        std::fs::create_dir_all(&staged)?;
        let mut moved = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            if PREPARED.iter().any(|p| name == *p) {
                continue;
            }
            std::fs::rename(entry.path(), staged.join(&name))?;
            moved.push(name);
        }
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            self.attach_prepared(sandbox),
        )
        .await
        .context("pool attachment timeout")
        .and_then(|r| r);
        // Put the files back before returning any attach error.
        for name in moved {
            std::fs::rename(staged.join(&name), root.join(&name))?;
        }
        std::fs::remove_dir(&staged)?;
        result
    }

    async fn attach_prepared(&self, sandbox: &Sandbox) -> Result<()> {
        // Lake's generated config files are part of the prepared snapshot.
        let prep = format!(
            "mkdir -p {WORK}/.lake && cp -a {}/.lake/config {WORK}/.lake/",
            image::PROJECT
        );
        let out = sandbox.command("bash", &["-c", &prep]).output().await?;
        if !out.status.success() {
            bail!(
                "prepare attach: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let config = format!("{WORK}/{SESSION_DIR}/pool.json");
        let out = sandbox
            .command(
                "python3",
                &[
                    "-m",
                    "scripts.beam_pool",
                    "attach",
                    "--root",
                    WORK,
                    "--endpoint",
                    &self.endpoint,
                    "--config",
                    &config,
                ],
            )
            .output()
            .await
            .context("beam-pool attach")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Require the snapshot descriptor in the output, not just exit 0.
        if !out.status.success() || !stdout.contains("\"snapshot\"") {
            bail!(
                "beam-pool attach failed: {}{}",
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// The MCP server command for an attached sandbox.
    pub fn mcp(&self, sandbox: &Sandbox) -> tokio::process::Command {
        sandbox.command(
            image::MCP,
            &["--lean-cmd", image::LEAN, "--lean-plugin", image::PLUGIN],
        )
    }
}
