//! The build plan - supercargo's real public interface.
//!
//! Emitters, the composite action, and anyone piping `supercargo plan` into
//! `jq` all consume this structure, so it is versioned independently of the
//! crate: bump [`PLAN_SCHEMA_VERSION`] on any breaking shape change.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Version of the serialized plan shape.
pub const PLAN_SCHEMA_VERSION: u32 = 1;

/// Everything a CI run needs to know about what to build and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    /// [`PLAN_SCHEMA_VERSION`] at computation time.
    pub schema_version: u32,
    /// Which strategy produced this plan.
    pub strategy: StrategyKind,
    /// Head commit the plan was computed at.
    pub head: String,
    /// Base commit for diff plans (the resolved merge-base), absent for hash
    /// plans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// One entry per image, sorted by image name.
    pub targets: Vec<Target>,
}

impl BuildPlan {
    /// Targets that actually need building.
    pub fn to_build(&self) -> impl Iterator<Item = &Target> {
        self.targets
            .iter()
            .filter(|t| matches!(t.decision, Decision::Build))
    }
}

/// The strategy a plan was computed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyKind {
    /// Changed-since-base planning.
    Diff,
    /// Registry-memoized input-hash planning.
    Hash,
}

/// One image: identity, how to build it, and whether to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    /// Image name without registry or tag, e.g. `acme/ingest`.
    pub name: String,
    /// Workspace crate the image packages.
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// Full image reference base (registry prefix + name, no tag).
    pub image: String,
    /// Dockerfile path relative to the workspace root.
    pub dockerfile: Utf8PathBuf,
    /// Docker build context relative to the workspace root.
    pub context: Utf8PathBuf,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// `--build-arg` values for the image build.
    pub build_args: BTreeMap<String, String>,
    /// Deterministic input hash (`h<scheme>-<hex>`). Computed for both
    /// strategies - in diff mode it is still the right immutable tag.
    pub input_hash: String,
    /// Tags to apply, in configured order (hash tag first by default).
    pub tags: Vec<String>,
    /// Whether to build or skip, and why (see `reasons`).
    pub decision: Decision,
    /// Human-readable explanations for the decision, e.g.
    /// `"crate common changed: crates/common/src/lib.rs"`.
    pub reasons: Vec<String>,
}

/// Build or skip, with enough context to retag on skip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Decision {
    /// The image must be rebuilt.
    Build,
    /// The image does not need rebuilding.
    Skip {
        /// Existing image reference carrying the same input hash, when the
        /// registry was probed - CI can retag it instead of rebuilding.
        #[serde(skip_serializing_if = "Option::is_none")]
        existing: Option<String>,
    },
}
