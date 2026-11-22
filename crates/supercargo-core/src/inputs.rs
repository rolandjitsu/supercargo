//! Resolution of one image's full input set: every path and non-path input
//! that, when changed, must invalidate the image.

use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};

use crate::workspace::{Closure, ImageTarget, Workspace};
use crate::{Error, Result};

/// Paths that invalidate every image regardless of configuration.
pub const IMPLICIT_GLOBAL_PATHS: &[&str] = &[
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo/config.toml",
];

/// Everything that determines one image's build output.
#[derive(Debug, Clone)]
pub struct InputSet {
    /// Input files and directories, relative to the workspace root: the
    /// closure's crate directories, the Dockerfile, extra paths, and global
    /// trigger paths.
    pub paths: BTreeSet<Utf8PathBuf>,
    /// Path prefixes (workspace-relative) removed from the input set.
    pub excludes: BTreeSet<Utf8PathBuf>,
    /// Resolved external packages (`name` -> versions) from the closure --
    /// the image's effective Cargo.lock subset.
    pub external_packages: BTreeMap<String, BTreeSet<String>>,
    /// Build args passed to the image build.
    pub build_args: BTreeMap<String, String>,
    /// Fingerprint of the root manifest with resolution-covered sections
    /// stripped (see [`manifest_fingerprint`]).
    pub manifest_fingerprint: String,
    /// The dependency closure the set was derived from.
    pub closure: Closure,
}

impl InputSet {
    /// Whether a workspace-relative path is excluded.
    pub fn is_excluded(&self, path: &Utf8Path) -> bool {
        self.excludes
            .iter()
            .any(|ex| path == ex || path.starts_with(ex))
    }
}

/// Resolve the input set for one image.
pub fn resolve(ws: &Workspace, target: &ImageTarget) -> Result<InputSet> {
    let closure = ws.closure(target)?;

    let manifest = ws.root().join(&target.crate_dir).join("Cargo.toml");
    let mut paths: BTreeSet<Utf8PathBuf> = closure.members.keys().cloned().collect();
    paths.insert(target.dockerfile.clone());
    for extra in &target.config.extra_paths {
        paths.insert(crate::workspace::normalize_ws_path(extra, &manifest)?);
    }
    paths.extend(IMPLICIT_GLOBAL_PATHS.iter().map(Utf8PathBuf::from));
    paths.extend(ws.config().global_paths.iter().cloned());

    let excludes = target
        .config
        .exclude_paths
        .iter()
        .map(|p| crate::workspace::normalize_ws_path(&target.crate_dir.join(p), &manifest))
        .collect::<crate::Result<_>>()?;

    let root_manifest = ws.root().join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&root_manifest)?;
    let manifest_fingerprint = manifest_fingerprint(&manifest_text, &root_manifest)?;

    Ok(InputSet {
        paths,
        excludes,
        external_packages: closure.external_packages.clone(),
        build_args: target.config.build_args.clone(),
        manifest_fingerprint,
        closure,
    })
}

/// Hash of the root manifest with the sections whose effects are already
/// captured elsewhere stripped out.
///
/// This is what keeps "edited the root Cargo.toml" from meaning "rebuild the
/// world": a `[workspace.dependencies]` bump shows up precisely through each
/// image's resolved package set, and member-list changes show up through the
/// closure's crate directories. What remains -- `[profile]`,
/// `[workspace.package]` fields like `edition`, `[workspace.lints]`, etc. --
/// genuinely affects every build, so any change to it invalidates every image.
pub fn manifest_fingerprint(manifest_text: &str, path: &Utf8Path) -> Result<String> {
    let mut value: toml::Value = toml::from_str(manifest_text).map_err(|e| Error::Parse {
        path: path.to_owned(),
        message: e.to_string(),
    })?;

    if let Some(workspace) = value.get_mut("workspace").and_then(|w| w.as_table_mut()) {
        // Covered by the resolved external package set (hash mode) and the
        // Cargo.lock diff (diff mode).
        workspace.remove("dependencies");
        // Covered by the closure's workspace crate directories.
        workspace.remove("members");
        workspace.remove("exclude");
        workspace.remove("default-members");
        // Supercargo's own config: changing the registry or tag scheme changes
        // the plan, not the artifact.
        workspace.remove("metadata");
    }
    // Patched sources re-pin packages, which the lockfile-derived inputs catch.
    if let Some(table) = value.as_table_mut() {
        table.remove("patch");
    }

    let normalized = toml::to_string(&value).map_err(|e| Error::Parse {
        path: path.to_owned(),
        message: format!("failed to re-serialize normalized manifest: {e}"),
    })?;
    Ok(blake3::hash(normalized.as_bytes()).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use crate::inputs::manifest_fingerprint;

    #[test]
    fn exclusion_matches_prefixes_only() {
        use std::collections::{BTreeMap, BTreeSet};

        use crate::inputs::InputSet;
        use crate::workspace::Closure;

        let set = InputSet {
            paths: BTreeSet::new(),
            excludes: BTreeSet::from(["crates/a/benches".into()]),
            external_packages: BTreeMap::new(),
            build_args: BTreeMap::new(),
            manifest_fingerprint: String::new(),
            closure: Closure::default(),
        };
        assert!(set.is_excluded(Utf8Path::new("crates/a/benches")));
        assert!(set.is_excluded(Utf8Path::new("crates/a/benches/big.rs")));
        assert!(!set.is_excluded(Utf8Path::new("crates/a/benches-other/file.rs")));
        assert!(!set.is_excluded(Utf8Path::new("crates/a/src/lib.rs")));
    }

    #[test]
    fn invalid_manifest_is_a_parse_error() {
        assert!(matches!(
            manifest_fingerprint("not = [valid", Utf8Path::new("Cargo.toml")),
            Err(crate::Error::Parse { .. })
        ));
    }

    #[test]
    fn dependency_bump_does_not_change_fingerprint() {
        let bumped = MANIFEST.replace("1.0.200", "1.0.203");
        assert_eq!(fp(MANIFEST), fp(&bumped));
    }

    #[test]
    fn member_list_change_does_not_change_fingerprint() {
        let fewer = MANIFEST.replace(", \"crates/b\"", "");
        assert_eq!(fp(MANIFEST), fp(&fewer));
    }

    #[test]
    fn profile_change_changes_fingerprint() {
        let fat = MANIFEST.replace("\"thin\"", "\"fat\"");
        assert_ne!(fp(MANIFEST), fp(&fat));
    }

    #[test]
    fn edition_change_changes_fingerprint() {
        let old = MANIFEST.replace("2024", "2021");
        assert_ne!(fp(MANIFEST), fp(&old));
    }

    const MANIFEST: &str = r#"
[workspace]
resolver = "3"
members = ["crates/a", "crates/b"]

[workspace.package]
edition = "2024"

[workspace.dependencies]
serde = "1.0.200"

[profile.release]
lto = "thin"
"#;

    fn fp(text: &str) -> String {
        manifest_fingerprint(text, Utf8Path::new("Cargo.toml")).unwrap()
    }
}
