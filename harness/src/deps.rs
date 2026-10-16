//! Sharing prebuilt dependencies (Mathlib) with verification copies.
//!
//! worktrees leaves `.lake` out of every snapshot, so a fresh copy would have to
//! rebuild Mathlib. Before Comparator runs, verify.rs links
//! `<copy>/.lake/packages` to the prepared packages directory. The link target
//! is outside the copy, and Comparator's Landlock sandbox only lets it write to
//! the copy's `.lake`, so the shared packages stay read-only.
//!
//! Agent sandboxes get the same directory as a read-only bind mount instead
//! (see sandbox.rs), because Beam's attach rejects a symlink that leaves the
//! project root.

use anyhow::{Context, Result, bail};
use std::path::Path;

/// Point `root/.lake/packages` at `packages`, which must be an absolute path.
pub fn link(root: &Path, packages: &Path) -> Result<()> {
    if !packages.join("mathlib").is_dir() && std::fs::read_dir(packages)?.next().is_none() {
        bail!("{} has no packages", packages.display());
    }
    let lake = root.join(".lake");
    std::fs::create_dir_all(&lake)?;
    let link = lake.join("packages");
    // A fresh root has no `.lake/packages`. If one exists (even a dangling link),
    // something is wrong, so fail instead of replacing it.
    match std::fs::symlink_metadata(&link) {
        Ok(_) => bail!("{} already exists", link.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::os::unix::fs::symlink(packages, &link)
        .with_context(|| format!("link {} -> {}", link.display(), packages.display()))
}
