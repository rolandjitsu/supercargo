//! Deterministic input hashing.
//!
//! The hash is the memoization key for hash-mode planning and the immutable
//! image tag for both modes, so its stability is a public contract:
//! [`HASH_SCHEME`] names the algorithm generation and is embedded in the
//! output (`h1-<hex>`). Bump it on *any* change to what is fed or how it is
//! framed -- every image then rebuilds once under the new scheme, visibly and
//! explicably, instead of silently colliding or silently never matching.

use camino::{Utf8Path, Utf8PathBuf};

use crate::inputs::InputSet;
use crate::vcs::Vcs;
use crate::{Error, Result};

/// Hash scheme generation.
pub const HASH_SCHEME: u32 = 1;

/// Hex digits of blake3 kept in the tag: 128 bits.
const HASH_LEN: usize = 32;

/// Translates between workspace-relative paths (what configuration and the
/// input set use) and repo-root-relative paths (what git commands take and
/// return), for
/// workspaces nested inside a larger repository. For the common
/// workspace-root-is-repo-root layout the prefix is empty.
#[derive(Debug, Clone)]
pub struct WsPrefix(Utf8PathBuf);

impl WsPrefix {
    /// Compute the prefix of `workspace_root` inside `repo_root`.
    pub fn new(workspace_root: &Utf8Path, repo_root: &Utf8Path) -> Result<Self> {
        // Canonicalize both sides: cargo and git disagree about symlinked
        // roots (macOS /var/folders vs /private/var/folders, for one).
        let ws = canonical(workspace_root);
        let repo = canonical(repo_root);
        let rel = ws.strip_prefix(&repo).map_err(|_| Error::Parse {
            path: workspace_root.to_owned(),
            message: format!("workspace root is not inside the git repository {repo_root}"),
        })?;
        Ok(Self(rel.to_owned()))
    }

    /// Workspace-relative -> repo-relative.
    pub fn to_repo(&self, path: &Utf8Path) -> Utf8PathBuf {
        if self.0.as_str().is_empty() {
            path.to_owned()
        } else {
            self.0.join(path)
        }
    }

    /// Repo-relative -> workspace-relative, when the path is under the
    /// workspace.
    pub fn to_workspace(&self, path: &Utf8Path) -> Option<Utf8PathBuf> {
        if self.0.as_str().is_empty() {
            Some(path.to_owned())
        } else {
            path.strip_prefix(&self.0).ok().map(Utf8Path::to_owned)
        }
    }
}

fn canonical(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8().unwrap_or_else(|_| path.to_owned())
}

/// Compute the input hash of one image at a revision.
///
/// File content enters through git blob OIDs (content plus file mode), never
/// through working-tree bytes, so the result is independent of mtimes,
/// checkout platform, and local filesystem state. Paths are hashed
/// workspace-relative, so the hash survives relocating the workspace inside a
/// larger repo. Callers that hash `HEAD` should gate on [`verify_clean`]
/// first.
pub fn input_hash(set: &InputSet, vcs: &dyn Vcs, rev: &str, prefix: &WsPrefix) -> Result<String> {
    let mut hasher = blake3::Hasher::new();

    // Length-framed records: no separator ambiguity between fields.
    let mut feed = |tag: &str, data: &str| {
        hasher.update(&(tag.len() as u64).to_le_bytes());
        hasher.update(tag.as_bytes());
        hasher.update(&(data.len() as u64).to_le_bytes());
        hasher.update(data.as_bytes());
    };

    feed("scheme", &HASH_SCHEME.to_string());
    feed("manifest", &set.manifest_fingerprint);

    let repo_paths: Vec<Utf8PathBuf> = set.paths.iter().map(|p| prefix.to_repo(p)).collect();
    for entry in vcs.tree_entries(rev, &repo_paths)? {
        let Some(ws_path) = prefix.to_workspace(&entry.path) else {
            continue;
        };
        if set.is_excluded(&ws_path) {
            continue;
        }
        feed("file", &format!("{} {} {}", entry.mode, entry.oid, ws_path));
    }

    for (name, versions) in &set.external_packages {
        for version in versions {
            feed("package", &format!("{name}@{version}"));
        }
    }

    for (key, value) in &set.build_args {
        feed("build-arg", &format!("{key}={value}"));
    }

    let hex = hasher.finalize().to_hex();
    Ok(format!("h{HASH_SCHEME}-{}", &hex.as_str()[..HASH_LEN]))
}

/// Refuse to hash a revision the working tree has drifted from.
///
/// Hashing reads committed git state; uncommitted edits under the input paths
/// would make the hash describe an image that is not what a build from this
/// tree produces.
pub fn verify_clean(set: &InputSet, vcs: &dyn Vcs, prefix: &WsPrefix) -> Result<()> {
    let repo_paths: Vec<Utf8PathBuf> = set.paths.iter().map(|p| prefix.to_repo(p)).collect();
    if vcs.is_dirty(&repo_paths)? {
        let first = set.paths.first().cloned().unwrap_or_default();
        return Err(Error::DirtyWorkTree(first));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};

    use crate::hash::WsPrefix;

    #[test]
    fn same_root_round_trips_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let prefix = WsPrefix::new(root, root).unwrap();
        assert_eq!(
            prefix.to_repo(Utf8Path::new("crates/a")),
            Utf8PathBuf::from("crates/a")
        );
        assert_eq!(
            prefix.to_workspace(Utf8Path::new("crates/a")),
            Some(Utf8PathBuf::from("crates/a"))
        );
    }

    #[test]
    fn nested_workspace_prefixes_and_strips() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let ws = root.join("services/monorepo");
        std::fs::create_dir_all(&ws).unwrap();

        let prefix = WsPrefix::new(&ws, root).unwrap();
        assert_eq!(
            prefix.to_repo(Utf8Path::new("crates/a")),
            Utf8PathBuf::from("services/monorepo/crates/a")
        );
        assert_eq!(
            prefix.to_workspace(Utf8Path::new("services/monorepo/crates/a")),
            Some(Utf8PathBuf::from("crates/a"))
        );
        assert_eq!(prefix.to_workspace(Utf8Path::new("unrelated/file")), None);
    }

    #[test]
    fn workspace_outside_the_repo_is_an_error() {
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let repo_root = Utf8Path::from_path(repo.path()).unwrap();
        let ws_root = Utf8Path::from_path(elsewhere.path()).unwrap();
        assert!(WsPrefix::new(ws_root, repo_root).is_err());
    }
}
