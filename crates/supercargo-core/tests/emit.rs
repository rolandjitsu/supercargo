//! Emitter snapshot tests over a hand-built plan: emitters are pure functions
//! of the plan, so no git or cargo is involved.

use std::collections::BTreeMap;

use supercargo_core::emit;
use supercargo_core::emit::gitlab;
use supercargo_core::plan::{BuildPlan, Decision, PLAN_SCHEMA_VERSION, StrategyKind, Target};

#[test]
fn plan_json_roundtrips() {
    let plan = sample_plan();
    let json = emit::plan_json(&plan);
    let parsed: BuildPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.targets.len(), plan.targets.len());
    assert_eq!(parsed.targets[0].decision, plan.targets[0].decision);
}

#[test]
fn github_output_snapshot() {
    insta::assert_snapshot!(emit::github::github_output(&sample_plan()));
}

#[test]
fn github_output_with_no_work() {
    let out = emit::github::github_output(&empty_plan());
    assert!(out.contains("has_work=false"));
    assert!(out.contains("count=0"));
}

#[test]
fn gitlab_pipeline_snapshot() {
    let yaml = gitlab::child_pipeline(&sample_plan(), &gitlab::Options::default());
    // JSON is valid YAML; GitLab consumes this directly.
    insta::assert_snapshot!(yaml);
}

#[test]
fn gitlab_pipeline_with_no_work_emits_a_noop_job() {
    let yaml = gitlab::child_pipeline(&empty_plan(), &gitlab::Options::default());
    assert!(yaml.contains("supercargo:nothing-to-build"));
    assert!(
        !yaml.contains("include"),
        "no templates needed for a no-op pipeline"
    );
}

fn sample_plan() -> BuildPlan {
    BuildPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        strategy: StrategyKind::Hash,
        head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        base: None,
        targets: vec![
            Target {
                name: "beta".to_owned(),
                crate_name: "svc-beta".to_owned(),
                image: "registry.example.com/demo/beta".to_owned(),
                dockerfile: "crates/svc-beta/Dockerfile".into(),
                context: ".".into(),
                build_args: BTreeMap::from([(
                    "PROFILE".to_owned(),
                    "release with space".to_owned(),
                )]),
                input_hash: "h1-00000000000000000000000000000bbb".to_owned(),
                tags: vec![
                    "h1-00000000000000000000000000000bbb".to_owned(),
                    "0123456789ab".to_owned(),
                ],
                decision: Decision::Skip {
                    existing: Some(
                        "registry.example.com/demo/beta:h1-00000000000000000000000000000bbb"
                            .to_owned(),
                    ),
                },
                reasons: vec!["registry already has it".to_owned()],
            },
            Target {
                name: "svc-alpha".to_owned(),
                crate_name: "svc-alpha".to_owned(),
                image: "registry.example.com/demo/svc-alpha".to_owned(),
                dockerfile: "crates/svc-alpha/Dockerfile".into(),
                context: ".".into(),
                build_args: BTreeMap::new(),
                input_hash: "h1-00000000000000000000000000000aaa".to_owned(),
                tags: vec![
                    "h1-00000000000000000000000000000aaa".to_owned(),
                    "0123456789ab".to_owned(),
                ],
                decision: Decision::Build,
                reasons: vec!["no image found for input hash".to_owned()],
            },
        ],
    }
}

fn empty_plan() -> BuildPlan {
    BuildPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        strategy: StrategyKind::Diff,
        head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        base: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
        targets: vec![Target {
            name: "beta".to_owned(),
            crate_name: "svc-beta".to_owned(),
            image: "registry.example.com/demo/beta".to_owned(),
            dockerfile: "crates/svc-beta/Dockerfile".into(),
            context: ".".into(),
            build_args: BTreeMap::new(),
            input_hash: "h1-00000000000000000000000000000bbb".to_owned(),
            tags: vec!["h1-00000000000000000000000000000bbb".to_owned()],
            decision: Decision::Skip { existing: None },
            reasons: vec!["no inputs changed since base".to_owned()],
        }],
    }
}

#[test]
fn gitlab_pipeline_honors_custom_template_and_stage() {
    let opts = gitlab::Options {
        template_include: "ci/supercargo-templates.yml".into(),
        stage: "images".into(),
        ..Default::default()
    };
    let yaml = gitlab::child_pipeline(&sample_plan(), &opts);
    assert!(yaml.contains("ci/supercargo-templates.yml"));
    assert!(yaml.contains("\"stage\": \"images\""));
    assert!(
        yaml.contains("retag:beta"),
        "skips with an existing ref become retag jobs"
    );
}
