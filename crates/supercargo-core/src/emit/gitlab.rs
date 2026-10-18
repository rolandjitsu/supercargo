//! GitLab CI emitter: a dynamic child pipeline.
//!
//! A plan job runs supercargo and saves the output as an artifact; a trigger
//! job includes it (`trigger: include: artifact:`, `strategy: depend`).
//! Supercargo stays out of the build itself: each emitted job `extends` a
//! template the repository owns (how to build is the user's business), and the
//! emitted pipeline `include`s the local file defining those templates.
//!
//! The output is serialized JSON. JSON is valid YAML, and this way quoting of
//! arbitrary build args and branch names is correct by construction.

use serde_json::{Map, Value, json};

use crate::plan::{BuildPlan, Decision};

/// Emitter settings; the defaults match the documented template file.
#[derive(Debug, Clone)]
pub struct Options {
    /// Repo-local file defining the job templates, pulled into the child
    /// pipeline via `include: local:`.
    pub template_include: String,
    /// Hidden job each build job extends.
    pub build_template: String,
    /// Hidden job each retag job extends (used for hash-mode skips that still
    /// need new tags on the existing image). Retag jobs are only emitted when
    /// a skipped target carries an existing reference.
    pub retag_template: String,
    /// Stage name for emitted jobs.
    pub stage: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            template_include: ".gitlab/supercargo.yml".to_owned(),
            build_template: ".supercargo-build".to_owned(),
            retag_template: ".supercargo-retag".to_owned(),
            stage: "build".to_owned(),
        }
    }
}

/// Render the child pipeline.
pub fn child_pipeline(plan: &BuildPlan, opts: &Options) -> String {
    let mut jobs = Map::new();

    for target in &plan.targets {
        let refs = crate::emit::image_refs(target);
        let mut variables = Map::new();
        variables.insert("SUPERCARGO_NAME".into(), json!(target.name));
        variables.insert("SUPERCARGO_CRATE".into(), json!(target.crate_name));
        variables.insert("SUPERCARGO_IMAGE".into(), json!(target.image));
        variables.insert(
            "SUPERCARGO_DOCKERFILE".into(),
            json!(target.dockerfile.as_str()),
        );
        variables.insert("SUPERCARGO_CONTEXT".into(), json!(target.context.as_str()));
        variables.insert("SUPERCARGO_INPUT_HASH".into(), json!(target.input_hash));
        variables.insert("SUPERCARGO_TAGS".into(), json!(refs.join(" ")));
        if !target.build_args.is_empty() {
            let flags = target
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
            variables.insert("SUPERCARGO_BUILD_ARGS".into(), json!(flags));
        }

        match &target.decision {
            Decision::Build => {
                jobs.insert(
                    format!("build:{}", target.name),
                    json!({
                        "extends": opts.build_template,
                        "stage": opts.stage,
                        "variables": Value::Object(variables),
                    }),
                );
            }
            Decision::Skip {
                existing: Some(existing),
            } => {
                variables.insert("SUPERCARGO_EXISTING".into(), json!(existing));
                jobs.insert(
                    format!("retag:{}", target.name),
                    json!({
                        "extends": opts.retag_template,
                        "stage": opts.stage,
                        "variables": Value::Object(variables),
                    }),
                );
            }
            Decision::Skip { existing: None } => {}
        }
    }

    let mut pipeline = Map::new();
    if jobs.is_empty() {
        // A child pipeline must contain at least one job.
        jobs.insert(
            "supercargo:nothing-to-build".into(),
            json!({
                "stage": opts.stage,
                "script": ["echo 'supercargo: no image inputs changed'"],
            }),
        );
    } else {
        pipeline.insert(
            "include".into(),
            json!([{ "local": opts.template_include }]),
        );
    }
    pipeline.insert("stages".into(), json!([opts.stage]));
    pipeline.extend(jobs);

    let mut out =
        serde_json::to_string_pretty(&Value::Object(pipeline)).expect("pipeline serializes");
    out.push('\n');
    out
}
