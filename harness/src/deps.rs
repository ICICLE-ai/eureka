//! Sharing prebuilt dependencies (Mathlib) with attempt and verification copies.
//!
//! worktrees excludes `.lake` from every snapshot, so a copy would rebuild all
//! dependencies. Instead the harness links `<copy>/.lake/packages` to one
//! trusted, prebuilt packages directory. The link target is never inside the
//! copy, so sandboxes that grant write access to the copy (bwrap for the shell,
//! Landlock for Comparator) leave the shared packages read-only.

use anyhow::{Context, Result, bail};
use std::path::Path;

pub fn link(root: &Path, packages: &Path) -> Result<()> {
    if !packages.join("mathlib").is_dir() && std::fs::read_dir(packages)?.next().is_none() {
        bail!("{} has no packages", packages.display());
    }
    let lake = root.join(".lake");
    std::fs::create_dir_all(&lake)?;
    let link = lake.join("packages");
    match std::fs::symlink_metadata(&link) {
        Ok(_) => bail!("{} already exists", link.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::os::unix::fs::symlink(packages, &link)
        .with_context(|| format!("link {} -> {}", link.display(), packages.display()))
}
