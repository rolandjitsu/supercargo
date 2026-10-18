//! Error type.

use camino::Utf8PathBuf;

/// Errors produced by supercargo-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `cargo metadata` / graph construction failed.
    #[error("failed to load cargo metadata: {0}")]
    Metadata(#[from] guppy::Error),

    /// A `[.. .metadata.supercargo]` table is invalid.
    #[error("invalid supercargo configuration in {manifest}: {message}")]
    Config {
        /// Manifest carrying the bad table.
        manifest: Utf8PathBuf,
        /// What is wrong with it.
        message: String,
    },

    /// A git command exited nonzero.
    #[error("git {args:?} failed: {stderr}")]
    Git {
        /// Arguments git was invoked with.
        args: Vec<String>,
        /// Trimmed stderr.
        stderr: String,
    },

    /// An external command could not be spawned.
    #[error("failed to run {command}: {source}")]
    Spawn {
        /// The command line.
        command: String,
        /// The spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// A file or command output could not be parsed.
    #[error("failed to parse {path}: {message}")]
    Parse {
        /// What was being parsed.
        path: Utf8PathBuf,
        /// Why it failed.
        message: String,
    },

    /// A registry request failed in a way that is neither "exists" nor
    /// "missing" (auth, network, unexpected status).
    #[error("registry probe failed for {image}: {message}")]
    Registry {
        /// The image reference or URL being probed.
        image: String,
        /// What went wrong.
        message: String,
    },

    /// An `--only` / `explain` / `hash` argument names no known image.
    #[error("no image named {0} is defined in this workspace")]
    UnknownImage(String),

    /// Hashing was requested on a working tree that differs from HEAD.
    #[error(
        "working tree has uncommitted changes under {0}; supercargo hashes committed git state \
         (commit, stash, or pass --allow-dirty to hash HEAD anyway)"
    )]
    DirtyWorkTree(Utf8PathBuf),

    /// Filesystem access failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}
