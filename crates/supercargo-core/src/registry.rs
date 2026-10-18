//! Registry existence probes for hash-mode planning.
//!
//! Hash mode checks, per image, whether the registry already holds a manifest
//! for the input hash. Two probe families cover that check:
//!
//! - [`HttpProbe`] (the default) uses the Docker Registry HTTP API v2
//!   directly, with credentials from the standard docker and podman auth
//!   files, including credential helpers and OS credential stores. No extra
//!   tool is needed in the CI job.
//! - [`CommandProbe`] runs a CLI the job already has and is logged in with:
//!   crane, skopeo, docker, or podman (skopeo covers buildah setups).
//!
//! Probes are read-only; supercargo never writes to a registry.

pub mod http;

pub use http::HttpProbe;

use std::process::{Command, Stdio};

use crate::{Error, Result};

/// Checks whether an image reference exists in its registry.
pub trait RegistryProbe {
    /// Check whether `image_ref` (including tag) exists.
    fn exists(&self, image_ref: &str) -> Result<bool>;
}

/// Probe that never finds an existing image: every target builds. Useful for
/// bootstrapping and for forcing full rebuilds.
#[derive(Debug, Default)]
pub struct NeverExists;

impl RegistryProbe for NeverExists {
    fn exists(&self, _image_ref: &str) -> Result<bool> {
        Ok(false)
    }
}

/// Probe that runs an external command, substituting `{ref}` with the image
/// reference. Exit code 0 means the image exists; any failure (missing tag,
/// auth problem, network) means "build it" -- the safe direction.
#[derive(Debug, Clone)]
pub struct CommandProbe {
    /// Command and arguments; each `{ref}` occurrence is replaced.
    pub command: Vec<String>,
}

impl CommandProbe {
    /// `crane manifest {ref}`.
    pub fn crane() -> Self {
        Self::from_words(&["crane", "manifest", "{ref}"])
    }

    /// `skopeo inspect --raw docker://{ref}`.
    pub fn skopeo() -> Self {
        Self::from_words(&["skopeo", "inspect", "--raw", "docker://{ref}"])
    }

    /// `docker manifest inspect {ref}`.
    pub fn docker() -> Self {
        Self::from_words(&["docker", "manifest", "inspect", "{ref}"])
    }

    /// `podman manifest inspect docker://{ref}`.
    pub fn podman() -> Self {
        Self::from_words(&["podman", "manifest", "inspect", "docker://{ref}"])
    }

    fn from_words(words: &[&str]) -> Self {
        Self {
            command: words.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl RegistryProbe for CommandProbe {
    fn exists(&self, image_ref: &str) -> Result<bool> {
        let args: Vec<String> = self
            .command
            .iter()
            .map(|part| part.replace("{ref}", image_ref))
            .collect();
        let (program, rest) = args.split_first().ok_or_else(|| Error::Config {
            manifest: "probe command".into(),
            message: "probe command is empty".to_owned(),
        })?;
        let status = Command::new(program)
            .args(rest)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|source| Error::Spawn {
                command: args.join(" "),
                source,
            })?;
        Ok(status.success())
    }
}

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::registry::{CommandProbe, NeverExists, RegistryProbe};

    #[test]
    fn never_exists_always_builds() {
        assert!(!NeverExists.exists("any/ref:tag").unwrap());
    }

    #[test]
    fn command_probe_substitutes_and_maps_exit_codes() {
        // `test a = b` exits 0 on equality: existence iff {ref} matches.
        let probe = CommandProbe {
            command: vec![
                "test".into(),
                "{ref}".into(),
                "=".into(),
                "img:h1-abc".into(),
            ],
        };
        assert!(probe.exists("img:h1-abc").unwrap());
        assert!(!probe.exists("img:h1-other").unwrap());
    }

    #[test]
    fn missing_probe_binary_is_an_error_not_a_skip() {
        let probe = CommandProbe {
            command: vec!["supercargo-does-not-exist-9z".into()],
        };
        assert!(matches!(probe.exists("img:tag"), Err(Error::Spawn { .. })));
    }

    #[test]
    fn empty_probe_command_is_a_config_error() {
        let probe = CommandProbe { command: vec![] };
        assert!(matches!(probe.exists("img:tag"), Err(Error::Config { .. })));
    }

    #[test]
    fn builtin_probes_reference_their_tools() {
        assert_eq!(CommandProbe::crane().command[0], "crane");
        assert_eq!(CommandProbe::skopeo().command[0], "skopeo");
        assert_eq!(CommandProbe::docker().command[0], "docker");
        assert_eq!(CommandProbe::podman().command[0], "podman");
        assert!(
            CommandProbe::crane()
                .command
                .iter()
                .any(|p| p.contains("{ref}"))
        );
    }
}
