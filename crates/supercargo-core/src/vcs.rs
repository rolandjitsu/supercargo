//! Version-control access, abstracted behind [`Vcs`].
//!
//! Supercargo hashes *committed* git state rather than re-reading file contents:
//! git already content-addressed every file at checkout, so a blob OID is a
//! free, mtime-immune, platform-canonical content hash. [`GitCli`] runs the
//! `git` CLI, which is always present in CI; the trait exists for tests, not
//! for alternative git implementations.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

use crate::{Error, Result};

/// One file in a git tree: path (relative to the repo root), file mode, and
/// blob object id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeEntry {
    /// Path relative to the repo root.
    pub path: Utf8PathBuf,
    /// Git file mode (100644, 100755, 120000, 160000).
    pub mode: String,
    /// Blob (or gitlink) object id.
    pub oid: String,
}

/// The VCS operations supercargo needs. All paths are relative to the
/// repository root.
pub trait Vcs {
    /// Absolute path of the repository root.
    fn repo_root(&self) -> &Utf8Path;

    /// Resolve a revision to a full commit sha.
    fn resolve(&self, rev: &str) -> Result<String>;

    /// Merge base of two revisions.
    fn merge_base(&self, a: &str, b: &str) -> Result<String>;

    /// Files that differ between two revisions.
    fn changed_files(&self, base: &str, head: &str) -> Result<Vec<Utf8PathBuf>>;

    /// All blobs at `rev` under the given paths (files or directories),
    /// sorted by path. Paths that do not exist at `rev` yield no entries.
    fn tree_entries(&self, rev: &str, paths: &[Utf8PathBuf]) -> Result<Vec<TreeEntry>>;

    /// Contents of a file at a revision. `Ok(None)` when the file does not
    /// exist at that revision.
    fn file_at(&self, rev: &str, path: &Utf8Path) -> Result<Option<String>>;

    /// Whether the working tree or index has uncommitted changes under the
    /// given paths.
    fn is_dirty(&self, paths: &[Utf8PathBuf]) -> Result<bool>;
}

/// [`Vcs`] implemented by shelling out to the `git` CLI.
#[derive(Debug)]
pub struct GitCli {
    repo_root: Utf8PathBuf,
}

impl GitCli {
    /// Discover the repository containing `dir`.
    pub fn discover(dir: &Utf8Path) -> Result<Self> {
        let root = run_git(dir, &["rev-parse", "--show-toplevel"])?;
        Ok(Self {
            repo_root: Utf8PathBuf::from(root.trim_end()),
        })
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        run_git(&self.repo_root, args)
    }
}

impl Vcs for GitCli {
    fn repo_root(&self) -> &Utf8Path {
        &self.repo_root
    }

    fn resolve(&self, rev: &str) -> Result<String> {
        // ^{commit} peels annotated tags so the plan always records a commit.
        let spec = format!("{rev}^{{commit}}");
        Ok(self
            .git(&["rev-parse", "--verify", &spec])?
            .trim_end()
            .to_owned())
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        Ok(self.git(&["merge-base", a, b])?.trim_end().to_owned())
    }

    fn changed_files(&self, base: &str, head: &str) -> Result<Vec<Utf8PathBuf>> {
        let out = self.git(&["diff", "--name-only", "-z", base, head])?;
        Ok(out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(Utf8PathBuf::from)
            .collect())
    }

    fn tree_entries(&self, rev: &str, paths: &[Utf8PathBuf]) -> Result<Vec<TreeEntry>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", rev, "--"];
        args.extend(paths.iter().map(|p| p.as_str()));
        let out = self.git(&args)?;

        // Each record is "<mode> <type> <oid>\t<path>" NUL-terminated.
        let mut entries = Vec::new();
        for record in out.split('\0').filter(|s| !s.is_empty()) {
            let (meta, path) = record.split_once('\t').ok_or_else(|| Error::Parse {
                path: "git ls-tree output".into(),
                message: format!("record without tab: {record:?}"),
            })?;
            let mut fields = meta.split(' ');
            let (mode, kind, oid) = match (fields.next(), fields.next(), fields.next()) {
                (Some(m), Some(k), Some(o)) => (m, k, o),
                _ => {
                    return Err(Error::Parse {
                        path: "git ls-tree output".into(),
                        message: format!("malformed record: {record:?}"),
                    });
                }
            };
            // Submodules (commit entries) have no blob content to hash; the
            // gitlink oid still participates, which is exactly right: the
            // pinned submodule commit *is* the input.
            if kind == "blob" || kind == "commit" {
                entries.push(TreeEntry {
                    path: Utf8PathBuf::from(path),
                    mode: mode.to_owned(),
                    oid: oid.to_owned(),
                });
            }
        }
        entries.sort();
        entries.dedup();
        Ok(entries)
    }

    fn file_at(&self, rev: &str, path: &Utf8Path) -> Result<Option<String>> {
        let spec = format!("{rev}:{path}");
        match self.git(&["show", &spec]) {
            Ok(contents) => Ok(Some(contents)),
            Err(Error::Git { stderr, .. })
                if stderr.contains("does not exist")
                    || stderr.contains("exists on disk, but not in") =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn is_dirty(&self, paths: &[Utf8PathBuf]) -> Result<bool> {
        if paths.is_empty() {
            return Ok(false);
        }
        let mut args = vec!["status", "--porcelain", "-z", "--"];
        args.extend(paths.iter().map(|p| p.as_str()));
        Ok(!self.git(&args)?.trim().is_empty())
    }
}

fn run_git(cwd: &Utf8Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|source| Error::Spawn {
            command: format!("git {}", args.join(" ")),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::Git {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|e| Error::Parse {
        path: "git output".into(),
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};

    use crate::Error;
    use crate::vcs::{GitCli, Vcs};

    #[test]
    fn resolve_merge_base_and_changed_files() {
        let (dir, git) = repo();
        let root = git.repo_root().to_owned();
        let base = git.resolve("HEAD").unwrap();
        assert_eq!(base.len(), 40);

        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        run(&root, &["commit", "-aqm", "change a"]);
        let head = git.resolve("HEAD").unwrap();

        assert_eq!(git.merge_base(&base, &head).unwrap(), base);
        assert_eq!(
            git.changed_files(&base, &head).unwrap(),
            vec![Utf8PathBuf::from("a.txt")]
        );
        drop(dir);
    }

    #[test]
    fn tree_entries_lists_blobs_under_paths() {
        let (_dir, git) = repo();
        let entries = git
            .tree_entries("HEAD", &["sub".into(), "a.txt".into()])
            .unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "sub/b.txt"]);
        assert!(
            entries
                .iter()
                .all(|e| e.mode == "100644" && e.oid.len() == 40)
        );

        // Nonexistent paths yield no entries; no paths, no invocation.
        assert!(
            git.tree_entries("HEAD", &["missing".into()])
                .unwrap()
                .is_empty()
        );
        assert!(git.tree_entries("HEAD", &[]).unwrap().is_empty());
    }

    #[test]
    fn file_at_returns_none_for_missing_files() {
        let (_dir, git) = repo();
        assert_eq!(
            git.file_at("HEAD", Utf8Path::new("a.txt"))
                .unwrap()
                .unwrap(),
            "one\n"
        );
        assert_eq!(
            git.file_at("HEAD", Utf8Path::new("nope.txt")).unwrap(),
            None
        );
    }

    #[test]
    fn is_dirty_sees_uncommitted_changes_under_paths() {
        let (_dir, git) = repo();
        let root = git.repo_root().to_owned();
        assert!(!git.is_dirty(&["a.txt".into()]).unwrap());
        std::fs::write(root.join("a.txt"), "dirty\n").unwrap();
        assert!(git.is_dirty(&["a.txt".into()]).unwrap());
        assert!(!git.is_dirty(&["sub".into()]).unwrap());
        assert!(!git.is_dirty(&[]).unwrap());
    }

    #[test]
    fn bad_revisions_are_errors() {
        let (_dir, git) = repo();
        assert!(matches!(git.resolve("not-a-ref"), Err(Error::Git { .. })));
    }

    fn repo() -> (tempfile::TempDir, GitCli) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        run(&path, &["init", "-q", "-b", "main"]);
        std::fs::write(path.join("a.txt"), "one\n").unwrap();
        std::fs::create_dir(path.join("sub")).unwrap();
        std::fs::write(path.join("sub/b.txt"), "two\n").unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "-q", "-m", "init"]);
        let git = GitCli::discover(&path).unwrap();
        (dir, git)
    }

    fn run(cwd: &Utf8Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
}
