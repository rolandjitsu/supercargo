//! Configuration, read from `[workspace.metadata.supercargo]` and
//! `[package.metadata.supercargo]` tables so that a single `cargo metadata`
//! invocation delivers everything - no separate config file.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The key under `[workspace.metadata]` / `[package.metadata]`.
pub const METADATA_KEY: &str = "supercargo";

/// `[workspace.metadata.supercargo]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct WorkspaceConfig {
    /// Registry prefix prepended to every image name, e.g. `ghcr.io/acme`.
    pub registry: Option<String>,

    /// Paths (relative to the workspace root) that invalidate **every** image
    /// when they change: shared Dockerfile bases, `rust-toolchain.toml`, etc.
    ///
    /// `rust-toolchain.toml`, `rust-toolchain`, and `.cargo/config.toml` are
    /// always included implicitly.
    #[serde(default)]
    pub global_paths: Vec<Utf8PathBuf>,

    /// Which tags to attach to built images. Defaults to
    /// `["hash", "git-sha"]`.
    #[serde(default = "TagKind::default_set")]
    pub tag_with: Vec<TagKind>,

    /// Fallback Dockerfile (relative to the workspace root) for images whose
    /// crate does not carry its own.
    pub default_dockerfile: Option<Utf8PathBuf>,
}

/// A tag family attached to built images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagKind {
    /// The input hash, e.g. `h1-4f9c<hex>` - the immutable, memoizable tag.
    Hash,
    /// The head commit sha.
    GitSha,
    /// The sanitized branch name, when one is known.
    Branch,
    /// A floating `latest`.
    Latest,
}

impl TagKind {
    fn default_set() -> Vec<TagKind> {
        vec![TagKind::Hash, TagKind::GitSha]
    }
}

/// `[package.metadata.supercargo]` - presence of this table (even empty) is
/// what opts a binary crate in as an image.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ImageConfig {
    /// Image name. Defaults to the crate name.
    pub image: Option<String>,

    /// Dockerfile path relative to the crate directory. Defaults to
    /// `Dockerfile` next to the crate manifest, falling back to the
    /// workspace-level `default_dockerfile`.
    pub dockerfile: Option<Utf8PathBuf>,

    /// Docker build context relative to the workspace root. Defaults to the
    /// workspace root (the usual layout for workspace builds).
    pub context: Option<Utf8PathBuf>,

    /// Additional workspace crates whose binaries the image bundles (e.g. a
    /// Dockerfile building `--bin` targets from a second crate). Their full
    /// dependency closures are unioned into the input set.
    #[serde(default)]
    pub extra_crates: Vec<String>,

    /// Extra input paths relative to the workspace root - configs or assets
    /// baked into the image, or files reached via `include_str!`/build.rs
    /// outside the crate directory.
    #[serde(default)]
    pub extra_paths: Vec<Utf8PathBuf>,

    /// Paths relative to the crate directory excluded from the input set
    /// (e.g. `benches`, `README.md`).
    #[serde(default)]
    pub exclude_paths: Vec<Utf8PathBuf>,

    /// `--build-arg` values passed to the image build; part of the input hash.
    #[serde(default)]
    pub build_args: BTreeMap<String, String>,
}

/// Deserialize the `supercargo` entry out of a `[.. .metadata]` table.
///
/// Returns `Ok(None)` when the table has no `supercargo` key; unknown fields
/// inside the table are hard errors so typos fail loudly in `check` and CI.
pub fn from_metadata_table<T: serde::de::DeserializeOwned>(
    table: &serde_json::Value,
    manifest: &camino::Utf8Path,
) -> Result<Option<T>> {
    let Some(value) = table.get(METADATA_KEY) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|e| Error::Config {
            manifest: manifest.to_owned(),
            message: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use crate::Error;
    use crate::config::{ImageConfig, WorkspaceConfig, from_metadata_table};

    #[test]
    fn absent_table_is_none() {
        let table = serde_json::json!({ "other-tool": {} });
        let parsed: Option<ImageConfig> =
            from_metadata_table(&table, Utf8Path::new("Cargo.toml")).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn empty_table_uses_defaults() {
        let table = serde_json::json!({ "supercargo": {} });
        let parsed: ImageConfig = from_metadata_table(&table, Utf8Path::new("Cargo.toml"))
            .unwrap()
            .unwrap();
        assert!(parsed.image.is_none());
        assert!(parsed.extra_paths.is_empty());
    }

    #[test]
    fn unknown_fields_fail_loudly() {
        let table = serde_json::json!({ "supercargo": { "extra_pathz": ["typo"] } });
        let err =
            from_metadata_table::<ImageConfig>(&table, Utf8Path::new("Cargo.toml")).unwrap_err();
        match err {
            Error::Config { manifest, message } => {
                assert_eq!(manifest, "Cargo.toml");
                assert!(message.contains("extra_pathz"), "message: {message}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn workspace_config_parses_tag_kinds() {
        let table = serde_json::json!({ "supercargo": {
            "registry": "ghcr.io/acme",
            "tag_with": ["hash", "git-sha", "branch", "latest"],
        }});
        let parsed: WorkspaceConfig = from_metadata_table(&table, Utf8Path::new("Cargo.toml"))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.registry.as_deref(), Some("ghcr.io/acme"));
        assert_eq!(parsed.tag_with.len(), 4);
    }
}
