//! The cargo workspace as supercargo sees it: a guppy package graph plus the
//! supercargo configuration carried in `[workspace.metadata]` and
//! `[package.metadata]` tables, and the per-image dependency closures derived
//! from it.

use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use guppy::PackageId;
use guppy::graph::{BuildTargetId, DependencyDirection, PackageGraph, PackageMetadata};

use crate::config::{self, ImageConfig, WorkspaceConfig};
use crate::{Error, Result};

/// A workspace binary crate that packages into a Docker image, discovered via
/// its `[package.metadata.supercargo]` table.
#[derive(Debug, Clone)]
pub struct ImageTarget {
    /// Image name without the registry prefix.
    pub name: String,
    /// The owning crate.
    pub crate_name: String,
    /// Guppy package id, usable for graph queries.
    pub package_id: PackageId,
    /// Crate directory relative to the workspace root.
    pub crate_dir: Utf8PathBuf,
    /// Dockerfile path relative to the workspace root.
    pub dockerfile: Utf8PathBuf,
    /// Docker build context relative to the workspace root (`.` = root).
    pub context: Utf8PathBuf,
    /// The crate's `[package.metadata.supercargo]` table.
    pub config: ImageConfig,
}

/// The build-relevant dependency closure of one image: every transitive
/// dependency reachable without following dev-only edges.
#[derive(Debug, Clone, Default)]
pub struct Closure {
    /// Workspace members in the closure: crate directory (relative to the
    /// workspace root) -> crate name. Includes the image's own crate.
    pub members: BTreeMap<Utf8PathBuf, String>,
    /// Resolved external packages as `name` -> set of `version` pins. This
    /// *is* the image's Cargo.lock subset: guppy resolves from the same
    /// lockfile cargo builds with.
    pub external_packages: BTreeMap<String, BTreeSet<String>>,
}

impl Closure {
    /// The closure member owning a workspace-relative path, if any.
    pub fn owning_member(&self, path: &Utf8Path) -> Option<(&Utf8Path, &str)> {
        // Longest-prefix match so nested crate dirs attribute correctly.
        self.members
            .iter()
            .filter(|(dir, _)| path.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_str().len())
            .map(|(dir, name)| (dir.as_path(), name.as_str()))
    }
}

/// A loaded workspace: package graph + supercargo config.
pub struct Workspace {
    graph: PackageGraph,
    config: WorkspaceConfig,
    root: Utf8PathBuf,
}

impl Workspace {
    /// Load the workspace containing `manifest_path` (a Cargo.toml path or a
    /// directory) by running `cargo metadata`.
    pub fn load(manifest_path: Option<&Utf8Path>) -> Result<Self> {
        let mut cmd = guppy::MetadataCommand::new();
        if let Some(path) = manifest_path {
            cmd.manifest_path(path);
        }
        let graph = cmd.build_graph()?;
        Self::from_graph(graph)
    }

    /// Wrap an already-built package graph (used by tests).
    pub fn from_graph(graph: PackageGraph) -> Result<Self> {
        let ws = graph.workspace();
        let root = ws.root().to_owned();
        let manifest = root.join("Cargo.toml");
        let config =
            config::from_metadata_table::<WorkspaceConfig>(ws.metadata_table(), &manifest)?
                .unwrap_or_default();
        Ok(Self {
            graph,
            config,
            root,
        })
    }

    /// Absolute workspace root.
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Workspace-level supercargo configuration.
    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    /// The underlying guppy graph.
    pub fn graph(&self) -> &PackageGraph {
        &self.graph
    }

    /// Workspace members that opted in as images: binary crates carrying a
    /// `[package.metadata.supercargo]` table. Sorted by image name.
    pub fn images(&self) -> Result<Vec<ImageTarget>> {
        let mut targets = Vec::new();
        for package in self.graph.workspace().iter() {
            let Some(cfg) = config::from_metadata_table::<ImageConfig>(
                package.metadata_table(),
                package.manifest_path(),
            )?
            else {
                continue;
            };
            let has_binary = package
                .build_targets()
                .any(|t| matches!(t.id(), BuildTargetId::Binary(_)));
            if !has_binary {
                return Err(Error::Config {
                    manifest: package.manifest_path().to_owned(),
                    message: format!(
                        "crate {} has [package.metadata.supercargo] but no [[bin]] target",
                        package.name()
                    ),
                });
            }
            targets.push(self.image_target(&package, cfg)?);
        }
        targets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(targets)
    }

    /// Names of workspace members that define at least one binary target.
    pub fn binary_crates(&self) -> Vec<String> {
        self.graph
            .workspace()
            .iter()
            .filter(|p| {
                p.build_targets()
                    .any(|t| matches!(t.id(), BuildTargetId::Binary(_)))
            })
            .map(|p| p.name().to_string())
            .collect()
    }

    /// Find one image by name.
    pub fn image(&self, name: &str) -> Result<ImageTarget> {
        self.images()?
            .into_iter()
            .find(|t| t.name == name || t.crate_name == name)
            .ok_or_else(|| Error::UnknownImage(name.to_owned()))
    }

    fn image_target(&self, package: &PackageMetadata<'_>, cfg: ImageConfig) -> Result<ImageTarget> {
        let crate_dir = self.member_dir(package)?;
        let dockerfile = match &cfg.dockerfile {
            Some(path) => crate_dir.join(path),
            None => {
                let crate_local = crate_dir.join("Dockerfile");
                if self.root.join(&crate_local).exists() {
                    crate_local
                } else if let Some(fallback) = &self.config.default_dockerfile {
                    fallback.clone()
                } else {
                    return Err(Error::Config {
                        manifest: package.manifest_path().to_owned(),
                        message: format!(
                            "no Dockerfile for image {}: add {crate_local}, set \
                             package.metadata.supercargo.dockerfile, or set \
                             workspace.metadata.supercargo.default_dockerfile",
                            package.name()
                        ),
                    });
                }
            }
        };
        Ok(ImageTarget {
            name: cfg
                .image
                .clone()
                .unwrap_or_else(|| package.name().to_string()),
            crate_name: package.name().to_string(),
            package_id: package.id().clone(),
            crate_dir,
            dockerfile,
            context: cfg
                .context
                .clone()
                .unwrap_or_else(|| Utf8PathBuf::from(".")),
            config: cfg,
        })
    }

    /// Compute the dependency closure of one image, excluding dev-only edges:
    /// a change to a dev-dependency (or a crate reachable only through one)
    /// must not rebuild images.
    pub fn closure(&self, target: &ImageTarget) -> Result<Closure> {
        let set = self
            .graph
            .query_forward([&target.package_id])?
            .resolve_with_fn(|_, link| !link.dev_only());

        let mut closure = Closure::default();
        for package in set.packages(DependencyDirection::Forward) {
            if package.in_workspace() {
                closure
                    .members
                    .insert(self.member_dir(&package)?, package.name().to_string());
            } else {
                closure
                    .external_packages
                    .entry(package.name().to_string())
                    .or_default()
                    .insert(package.version().to_string());
            }
        }
        Ok(closure)
    }

    /// A workspace member's directory, relative to the workspace root.
    fn member_dir(&self, package: &PackageMetadata<'_>) -> Result<Utf8PathBuf> {
        let dir = package
            .manifest_path()
            .parent()
            .unwrap_or(package.manifest_path());
        let rel = dir.strip_prefix(&self.root).map_err(|_| Error::Config {
            manifest: package.manifest_path().to_owned(),
            message: "workspace members outside the workspace root are not supported".to_owned(),
        })?;
        if rel.as_str().is_empty() {
            return Err(Error::Config {
                manifest: package.manifest_path().to_owned(),
                message: "a package at the workspace root would make every file an input; \
                          move crates into subdirectories"
                    .to_owned(),
            });
        }
        Ok(rel.to_owned())
    }
}
