//! Cargo.lock parsing and diffing, used by the diff strategy to map external
//! dependency changes to affected images at *package* granularity: bumping one
//! `[workspace.dependencies]` entry only touches images whose resolved
//! dependency closure contains that package.

use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8PathBuf;
use serde::Deserialize;

use crate::{Error, Result};

/// The subset of Cargo.lock supercargo reads.
#[derive(Debug, Deserialize)]
struct Lockfile {
    #[serde(default)]
    package: Vec<LockPackage>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    #[serde(default)]
    source: Option<String>,
}

/// Resolved packages keyed by name; a name can resolve at several versions.
/// The value set holds `version[@source]` strings so a registry -> git source
/// switch at the same version still registers as a change.
pub type Packages = BTreeMap<String, BTreeSet<String>>;

/// Parse a Cargo.lock into its package map.
pub fn parse(text: &str) -> Result<Packages> {
    let lock: Lockfile = toml::from_str(text).map_err(|e| Error::Parse {
        path: Utf8PathBuf::from("Cargo.lock"),
        message: e.to_string(),
    })?;
    let mut packages = Packages::new();
    for p in lock.package {
        let pin = match &p.source {
            Some(source) => format!("{}@{source}", p.version),
            None => p.version.clone(),
        };
        packages.entry(p.name).or_default().insert(pin);
    }
    Ok(packages)
}

/// Names whose resolution differs between two lockfiles: added, removed, or
/// re-pinned packages.
pub fn changed_packages(base: &Packages, head: &Packages) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    for (name, pins) in head {
        if base.get(name) != Some(pins) {
            changed.insert(name.clone());
        }
    }
    for name in base.keys() {
        if !head.contains_key(name) {
            changed.insert(name.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use crate::lockfile::{changed_packages, parse};

    const BASE: &str = r#"
version = 4

[[package]]
name = "svc-alpha"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "left-pad"
version = "0.9.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn version_bump_is_detected() {
        let head = BASE.replace("1.0.200", "1.0.203");
        let changed = changed_packages(&parse(BASE).unwrap(), &parse(&head).unwrap());
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec!["serde"]);
    }

    #[test]
    fn source_switch_at_same_version_is_detected() {
        let head = BASE.replace(
            "registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"left-pad",
            "git+https://github.com/serde-rs/serde?rev=abc123\"\n\n[[package]]\nname = \"left-pad",
        );
        let changed = changed_packages(&parse(BASE).unwrap(), &parse(&head).unwrap());
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec!["serde"]);
    }

    #[test]
    fn removal_and_addition_are_detected() {
        let head = BASE.replace("left-pad", "right-pad");
        let changed = changed_packages(&parse(BASE).unwrap(), &parse(&head).unwrap());
        assert_eq!(
            changed.into_iter().collect::<Vec<_>>(),
            vec!["left-pad", "right-pad"]
        );
    }

    #[test]
    fn identical_lockfiles_change_nothing() {
        assert!(changed_packages(&parse(BASE).unwrap(), &parse(BASE).unwrap()).is_empty());
    }

    #[test]
    fn duplicate_versions_of_one_name_are_tracked_together() {
        let head = format!(
            "{BASE}\n[[package]]\nname = \"serde\"\nversion = \"2.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        );
        let changed = changed_packages(&parse(BASE).unwrap(), &parse(&head).unwrap());
        assert_eq!(changed.into_iter().collect::<Vec<_>>(), vec!["serde"]);
    }
}
