//! GitHub Actions emitter.
//!
//! GitHub has no dynamic pipelines, so the shape is matrix-from-JSON: a plan
//! job runs supercargo and publishes outputs; the build job fans out with
//! `strategy.matrix: ${{ fromJson(needs.plan.outputs.matrix) }}`. An empty
//! matrix fails a job, hence the separate `has_work` output to gate on.

use std::fmt::Write as _;

use serde::Serialize;

use crate::plan::{BuildPlan, Decision, Target};

/// One matrix entry. Field names are the `matrix.*` keys build steps see.
#[derive(Debug, Serialize)]
pub struct MatrixEntry {
    /// Image name without registry.
    pub name: String,
    /// Owning workspace crate.
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// Full image reference base (no tag).
    pub image: String,
    /// Dockerfile path, workspace-relative.
    pub dockerfile: String,
    /// Build context, workspace-relative.
    pub context: String,
    /// Deterministic input hash.
    pub input_hash: String,
    /// Full references (`image:tag`), ready for `-t`.
    pub tags: Vec<String>,
    /// The same, comma-separated -- the format docker/build-push-action takes.
    pub tags_csv: String,
    /// Shell-quoted `--build-arg K=V ...` string, empty when there are none.
    pub build_args_flags: String,
}

/// The matrix object: `{"include": [...]}`.
#[derive(Debug, Serialize)]
pub struct Matrix {
    /// Matrix entries, one per image to build.
    pub include: Vec<MatrixEntry>,
}

/// Build the matrix of targets that need building.
pub fn matrix(plan: &BuildPlan) -> Matrix {
    Matrix {
        include: plan.to_build().map(entry).collect(),
    }
}

fn entry(target: &Target) -> MatrixEntry {
    let tags = crate::emit::image_refs(target);
    let build_args_flags = target
        .build_args
        .iter()
        .map(|(k, v)| {
            format!(
                "--build-arg {}",
                crate::emit::shell_quote(&format!("{k}={v}"))
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    MatrixEntry {
        name: target.name.clone(),
        crate_name: target.crate_name.clone(),
        image: target.image.clone(),
        dockerfile: target.dockerfile.to_string(),
        context: target.context.to_string(),
        input_hash: target.input_hash.clone(),
        tags_csv: tags.join(","),
        tags,
        build_args_flags,
    }
}

/// Render the `$GITHUB_OUTPUT` lines: `matrix` (the JSON), `has_work`,
/// `count`, and `skipped` (JSON list of `{name, existing}` for retagging).
pub fn github_output(plan: &BuildPlan) -> String {
    let m = matrix(plan);
    let skipped: Vec<serde_json::Value> = plan
        .targets
        .iter()
        .filter_map(|t| match &t.decision {
            Decision::Skip {
                existing: Some(existing),
            } => Some(serde_json::json!({
                "name": t.name,
                "image": t.image,
                "existing": existing,
                "tags": crate::emit::image_refs(t),
            })),
            _ => None,
        })
        .collect();

    let mut out = String::new();
    let _ = writeln!(out, "has_work={}", !m.include.is_empty());
    let _ = writeln!(out, "count={}", m.include.len());
    let _ = writeln!(out, "matrix<<SUPERCARGO_EOF");
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string(&m).expect("matrix serializes")
    );
    let _ = writeln!(out, "SUPERCARGO_EOF");
    let _ = writeln!(out, "skipped<<SUPERCARGO_EOF");
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string(&skipped).expect("skipped serializes")
    );
    let _ = writeln!(out, "SUPERCARGO_EOF");
    out
}
