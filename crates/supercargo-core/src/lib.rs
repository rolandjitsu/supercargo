//! Core library for supercargo.
//!
//! Supercargo answers one question for a Rust monorepo that packages workspace
//! binaries into Docker images: **which images actually need rebuilding?**
//!
//! It computes, per image, the full set of build inputs - the owning crate's
//! sources, every transitive workspace dependency (dev-dependencies excluded),
//! the resolved external dependency set, the Dockerfile, declared extra paths,
//! and global trigger files - and turns that into a [`plan::BuildPlan`] using
//! one of two strategies:
//!
//! - **diff**: compare against a base git ref (merge-base for PRs) and mark an
//!   image for rebuild when any of its inputs changed.
//! - **hash**: compute a deterministic input hash and skip the build when the
//!   image registry already holds a manifest for that hash. The registry acts
//!   as the memoization table, which requires build jobs to push the hash tag;
//!   diff mode involves no registry at all.
//!
//! The plan - not any particular CI system - is the public interface. Emitters
//! in [`emit`] serialize a plan for GitHub Actions (matrix JSON) or GitLab CI
//! (dynamic child pipeline); the plain JSON form is stable and versioned via
//! [`plan::PLAN_SCHEMA_VERSION`] so it can be consumed by anything else.

#![warn(missing_docs)]

pub mod config;
pub mod emit;
pub mod error;
pub mod hash;
pub mod inputs;
pub mod lockfile;
pub mod plan;
pub mod registry;
pub mod strategy;
pub mod vcs;
pub mod workspace;

/// The crate error type.
pub use error::Error;

/// Convenience alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
