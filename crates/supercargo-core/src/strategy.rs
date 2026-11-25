//! Plan computation: the diff and hash strategies.

use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use crate::config::TagKind;
use crate::hash::{self, WsPrefix};
use crate::inputs::{self, IMPLICIT_GLOBAL_PATHS, InputSet};
use crate::plan::{BuildPlan, Decision, StrategyKind, Target};
use crate::registry::RegistryProbe;
use crate::vcs::Vcs;
use crate::workspace::{ImageTarget, Workspace};
use crate::{Error, Result};

/// Reasons kept per target before truncating; a tree-wide refactor should not
/// turn the plan into a changelog.
const MAX_REASONS: usize = 8;

/// How to decide build-vs-skip.
pub enum Strategy<'a> {
    /// Rebuild images whose inputs changed since (the merge-base with) `base`.
    Diff {
        /// Base ref: a branch, sha, or symbolic ref. The plan records the
        /// resolved merge-base with HEAD.
        base: String,
        /// Treat `base` as the merge base itself and diff against it directly.
        /// CI systems often provide the true merge base (GitLab's
        /// `CI_MERGE_REQUEST_DIFF_BASE_SHA`); re-deriving it with
        /// `git merge-base` can fail on shallow clones.
        base_is_merge_base: bool,
    },
    /// Rebuild images whose input hash is absent from the registry.
    ///
    /// The contract: build jobs push the hash tag. The registry is the
    /// memoization table, so a hash tag that is never pushed is never found,
    /// and nothing is ever skipped.
    Hash {
        /// Registry existence probe.
        probe: &'a dyn RegistryProbe,
    },
}

/// Options shared by both strategies.
#[derive(Debug, Default)]
pub struct PlanOptions {
    /// Restrict planning to these image (or crate) names; empty = all.
    pub only: Vec<String>,
    /// Mark every target for build regardless of strategy outcome.
    pub force_all: bool,
    /// Permit hashing HEAD from a dirty working tree (hash mode normally
    /// refuses: the hash describes committed state only).
    pub allow_dirty: bool,
    /// Branch name used for the `branch` tag kind, when known.
    pub branch: Option<String>,
}

/// Compute a build plan.
pub fn plan(
    ws: &Workspace,
    vcs: &dyn Vcs,
    strategy: &Strategy<'_>,
    opts: &PlanOptions,
) -> Result<BuildPlan> {
    let head = vcs.resolve("HEAD")?;
    let prefix = WsPrefix::new(ws.root(), vcs.repo_root())?;

    let mut targets = ws.images()?;
    if !opts.only.is_empty() {
        targets.retain(|t| {
            opts.only
                .iter()
                .any(|only| *only == t.name || *only == t.crate_name)
        });
        for only in &opts.only {
            if !targets
                .iter()
                .any(|t| t.name == *only || t.crate_name == *only)
            {
                return Err(Error::UnknownImage(only.clone()));
            }
        }
    }

    let diff_ctx = match strategy {
        Strategy::Diff {
            base,
            base_is_merge_base,
        } => Some(DiffContext::prepare(
            ws,
            vcs,
            &prefix,
            base,
            *base_is_merge_base,
            &head,
        )?),
        Strategy::Hash { .. } => None,
    };

    let mut planned = Vec::new();
    for target in &targets {
        let set = inputs::resolve(ws, target)?;
        if matches!(strategy, Strategy::Hash { .. }) && !opts.allow_dirty {
            hash::verify_clean(&set, vcs, &prefix)?;
        }
        let input_hash = hash::input_hash(&set, vcs, &head, &prefix)?;
        let image = match &ws.config().registry {
            Some(registry) => format!("{}/{}", registry.trim_end_matches('/'), target.name),
            None => target.name.clone(),
        };
        let tags = tags_for(ws, &input_hash, &head, opts);

        let (decision, reasons) = if opts.force_all {
            (Decision::Build, vec!["forced (--force-all)".to_owned()])
        } else {
            match strategy {
                Strategy::Diff { .. } => diff_ctx
                    .as_ref()
                    .expect("diff context prepared above")
                    .decide(target, &set),
                Strategy::Hash { probe } => decide_hash(*probe, &image, &input_hash)?,
            }
        };

        planned.push(Target {
            name: target.name.clone(),
            crate_name: target.crate_name.clone(),
            image,
            dockerfile: target.dockerfile.clone(),
            context: target.context.clone(),
            build_args: set.build_args.clone(),
            input_hash,
            tags,
            decision,
            reasons,
        });
    }

    Ok(BuildPlan {
        schema_version: crate::plan::PLAN_SCHEMA_VERSION,
        strategy: match strategy {
            Strategy::Diff { .. } => StrategyKind::Diff,
            Strategy::Hash { .. } => StrategyKind::Hash,
        },
        head,
        base: diff_ctx.map(|c| c.base),
        targets: planned,
    })
}

fn decide_hash(
    probe: &dyn RegistryProbe,
    image: &str,
    input_hash: &str,
) -> Result<(Decision, Vec<String>)> {
    let image_ref = format!("{image}:{input_hash}");
    if probe.exists(&image_ref)? {
        Ok((
            Decision::Skip {
                existing: Some(image_ref.clone()),
            },
            vec![format!("registry already has {image_ref}")],
        ))
    } else {
        Ok((
            Decision::Build,
            vec![format!("no image found for input hash {input_hash}")],
        ))
    }
}

/// Precomputed facts about the base..head diff, shared across targets.
struct DiffContext {
    /// Resolved merge-base sha.
    base: String,
    /// Changed files, workspace-relative. Changes outside the workspace
    /// prefix are irrelevant by construction: every input path is inside it.
    changed: Vec<Utf8PathBuf>,
    /// Package names whose Cargo.lock resolution changed.
    lock_changed: BTreeSet<String>,
    /// Whether the root manifest changed beyond the resolution-covered
    /// sections (see [`inputs::manifest_fingerprint`]).
    manifest_changed_globally: bool,
    global_paths: Vec<Utf8PathBuf>,
}

impl DiffContext {
    fn prepare(
        ws: &Workspace,
        vcs: &dyn Vcs,
        prefix: &WsPrefix,
        base_ref: &str,
        base_is_merge_base: bool,
        head: &str,
    ) -> Result<Self> {
        let base = if base_is_merge_base {
            vcs.resolve(base_ref)?
        } else {
            vcs.merge_base(base_ref, head)?
        };
        let changed: Vec<Utf8PathBuf> = vcs
            .changed_files(&base, head)?
            .iter()
            .filter_map(|p| prefix.to_workspace(p))
            .collect();

        let lock_path = prefix.to_repo(Utf8PathBuf::from("Cargo.lock").as_path());
        let lock_changed = if changed.iter().any(|p| p == "Cargo.lock") {
            let head_lock = vcs.file_at(head, &lock_path)?.unwrap_or_default();
            let head_packages = crate::lockfile::parse(&head_lock)?;
            match vcs.file_at(&base, &lock_path)? {
                Some(base_lock) => crate::lockfile::changed_packages(
                    &crate::lockfile::parse(&base_lock)?,
                    &head_packages,
                ),
                // No lockfile at base: every resolved package is new.
                None => head_packages.into_keys().collect(),
            }
        } else {
            BTreeSet::new()
        };

        let manifest_path = prefix.to_repo(Utf8PathBuf::from("Cargo.toml").as_path());
        let manifest_changed_globally = if changed.iter().any(|p| p == "Cargo.toml") {
            let head_fp = vcs
                .file_at(head, &manifest_path)?
                .map(|t| inputs::manifest_fingerprint(&t, &manifest_path))
                .transpose()?;
            let base_fp = vcs
                .file_at(&base, &manifest_path)?
                .map(|t| inputs::manifest_fingerprint(&t, &manifest_path))
                .transpose()?;
            head_fp != base_fp
        } else {
            false
        };

        let mut global_paths: Vec<Utf8PathBuf> = IMPLICIT_GLOBAL_PATHS
            .iter()
            .map(Utf8PathBuf::from)
            .collect();
        global_paths.extend(ws.config().global_paths.iter().cloned());

        Ok(Self {
            base,
            changed,
            lock_changed,
            manifest_changed_globally,
            global_paths,
        })
    }

    fn decide(&self, target: &ImageTarget, set: &InputSet) -> (Decision, Vec<String>) {
        let mut reasons = Vec::new();

        if self.manifest_changed_globally {
            reasons.push(
                "root Cargo.toml changed outside dependency/member sections (affects all images)"
                    .to_owned(),
            );
        }

        for path in &self.changed {
            if set.is_excluded(path) {
                continue;
            }
            if let Some((_, crate_name)) = set.closure.owning_member(path) {
                reasons.push(format!("crate {crate_name} changed: {path}"));
            } else if *path == target.dockerfile {
                reasons.push(format!("Dockerfile changed: {path}"));
            } else if target
                .config
                .extra_paths
                .iter()
                .any(|e| path == e || path.starts_with(e))
            {
                reasons.push(format!("image input changed: {path}"));
            } else if self
                .global_paths
                .iter()
                .any(|g| path == g || path.starts_with(g))
            {
                reasons.push(format!("global trigger changed: {path}"));
            }
        }

        for name in self.lock_changed.intersection(
            &set.external_packages
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
        ) {
            reasons.push(format!("dependency {name} changed in Cargo.lock"));
        }

        if reasons.len() > MAX_REASONS {
            let more = reasons.len() - MAX_REASONS;
            reasons.truncate(MAX_REASONS);
            reasons.push(format!("... and {more} more"));
        }

        if reasons.is_empty() {
            (
                Decision::Skip { existing: None },
                vec!["no inputs changed since base".to_owned()],
            )
        } else {
            (Decision::Build, reasons)
        }
    }
}

fn tags_for(ws: &Workspace, input_hash: &str, head: &str, opts: &PlanOptions) -> Vec<String> {
    let mut tags = Vec::new();
    for kind in &ws.config().tag_with {
        match kind {
            TagKind::Hash => tags.push(input_hash.to_owned()),
            TagKind::GitSha => tags.push(head.chars().take(12).collect()),
            TagKind::Branch => {
                if let Some(branch) = &opts.branch {
                    tags.push(sanitize_tag(branch));
                }
            }
            TagKind::Latest => tags.push("latest".to_owned()),
        }
    }
    tags.dedup();
    tags
}

/// Turn a branch name into a valid docker tag: allowed charset, no leading
/// separator, bounded length.
fn sanitize_tag(name: &str) -> String {
    let mut tag: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while tag.starts_with(['.', '-']) {
        tag.remove(0);
    }
    tag.truncate(128);
    if tag.is_empty() {
        "unnamed".to_owned()
    } else {
        tag
    }
}

#[cfg(test)]
mod tests {
    use crate::strategy::sanitize_tag;

    #[test]
    fn branch_names_become_valid_tags() {
        assert_eq!(sanitize_tag("feat/lock-subset"), "feat-lock-subset");
        assert_eq!(sanitize_tag("-weird..start"), "weird..start");
        assert_eq!(sanitize_tag("h\u{e9}llo"), "h-llo");
    }
}
