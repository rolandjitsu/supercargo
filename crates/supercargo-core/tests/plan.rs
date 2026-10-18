//! End-to-end planning tests against the demo fixture workspace
//! (tests/fixtures/demo at the repo root): copy it into a fresh git repo,
//! commit, mutate, and check what the planner decides.

use std::path::Path;
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use supercargo_core::Error as stevedore_err;
use supercargo_core::plan::{BuildPlan, Decision};
use supercargo_core::registry::{NeverExists, RegistryProbe};
use supercargo_core::strategy::{self, PlanOptions, Strategy};
use supercargo_core::vcs::GitCli;
use supercargo_core::workspace::Workspace;

#[test]
fn discovers_images_and_closures() {
    let repo = DemoRepo::new();
    let ws = repo.workspace();

    let images = ws.images().unwrap();
    let names: Vec<&str> = images.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["beta", "svc-alpha"]);

    let alpha = ws.image("svc-alpha").unwrap();
    let closure = ws.closure(&alpha).unwrap();
    let members: Vec<&str> = closure.members.values().map(String::as_str).collect();
    assert_eq!(
        members,
        ["common", "svc-alpha"],
        "dev-helper must not be in the closure"
    );
    assert!(closure.external_packages.contains_key("either"));

    let beta = ws.image("beta").unwrap();
    let closure = ws.closure(&beta).unwrap();
    assert!(
        !closure.external_packages.contains_key("either"),
        "beta does not depend on either"
    );
}

#[test]
fn hash_is_deterministic_across_clones() {
    let first = DemoRepo::new().hash_plan();
    let second = DemoRepo::new().hash_plan();
    for (a, b) in first.targets.iter().zip(&second.targets) {
        assert_eq!(
            a.input_hash, b.input_hash,
            "hash of {} differs between clones",
            a.name
        );
    }
}

#[test]
fn plan_snapshot() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("crates/svc-beta/src/main.rs", "// touched\n");
    repo.commit("feat: touch beta");

    let plan = repo.diff_plan(&base);
    insta::assert_json_snapshot!(plan, {
        ".head" => "[head]",
        ".base" => "[base]",
        ".targets[].tags[1]" => "[sha]",
    });
}

#[test]
fn crate_change_rebuilds_only_dependents() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("crates/svc-beta/src/main.rs", "// touched\n");
    repo.commit("feat: touch beta");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", false)]);
}

#[test]
fn shared_lib_change_rebuilds_all_dependents() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("crates/common/src/lib.rs", "// touched\n");
    repo.commit("feat: touch common");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", true)]);
    assert!(
        plan.targets[0]
            .reasons
            .iter()
            .any(|r| r.contains("crate common changed"))
    );
}

#[test]
fn dev_dep_change_rebuilds_nothing() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("crates/dev-helper/src/lib.rs", "// touched\n");
    repo.commit("chore: touch dev helper");

    assert_eq!(
        decisions(&repo.diff_plan(&base)),
        [("beta", false), ("svc-alpha", false)]
    );
}

#[test]
fn extra_path_change_rebuilds_its_image_only() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("configs/beta.yaml", "timeout: 5s\n");
    repo.commit("feat: tune beta config");

    assert_eq!(
        decisions(&repo.diff_plan(&base)),
        [("beta", true), ("svc-alpha", false)]
    );
}

#[test]
fn global_trigger_rebuilds_everything() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("docker/base.Dockerfile", "# bump\n");
    repo.commit("chore: bump base image");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", true)]);
    assert!(
        plan.targets[0]
            .reasons
            .iter()
            .any(|r| r.contains("global trigger"))
    );
}

#[test]
fn excluded_path_change_rebuilds_nothing() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append("crates/svc-alpha/benches/alpha.rs", "// slower\n");
    repo.commit("bench: tweak");

    assert_eq!(
        decisions(&repo.diff_plan(&base)),
        [("beta", false), ("svc-alpha", false)]
    );
}

#[test]
fn manifest_reformatting_rebuilds_nothing() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.append(
        "Cargo.toml",
        "\n# a trailing comment changes bytes, not meaning\n",
    );
    repo.commit("docs: comment the manifest");

    assert_eq!(
        decisions(&repo.diff_plan(&base)),
        [("beta", false), ("svc-alpha", false)]
    );
}

#[test]
fn profile_change_rebuilds_everything() {
    let repo = DemoRepo::new();
    let base = repo.head();
    repo.replace("Cargo.toml", "lto = \"thin\"", "lto = \"fat\"");
    repo.commit("perf: fat lto");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", true)]);
    assert!(
        plan.targets[0]
            .reasons
            .iter()
            .any(|r| r.contains("root Cargo.toml"))
    );
}

#[test]
fn lockfile_bump_rebuilds_only_dependents() {
    // Base commit carries a hand-rewound lockfile (either at a fake earlier
    // version); head restores the real one. Only the base side is parsed by
    // supercargo (via git show), so cargo never sees the fake version.
    let repo = DemoRepo::new();
    let real_lock = repo.read("Cargo.lock");
    let old_lock = real_lock.replace(
        &format!("name = \"either\"\nversion = \"{EITHER_VERSION}\""),
        "name = \"either\"\nversion = \"1.10.0\"",
    );
    assert_ne!(
        old_lock, real_lock,
        "fixture lockfile must pin either {EITHER_VERSION}"
    );
    repo.write("Cargo.lock", &old_lock);
    repo.commit("chore: rewind lockfile");
    let base = repo.head();
    repo.write("Cargo.lock", &real_lock);
    repo.commit("chore: bump either");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", false), ("svc-alpha", true)]);
    assert!(
        plan.targets[1]
            .reasons
            .iter()
            .any(|r| r.contains("dependency either changed")),
        "reasons: {:?}",
        plan.targets[1].reasons
    );
}

#[test]
fn hash_mode_probes_the_registry() {
    let repo = DemoRepo::new();

    let all_build = repo.hash_plan();
    assert_eq!(decisions(&all_build), [("beta", true), ("svc-alpha", true)]);

    // A registry that already has beta's hash: beta skips with the existing
    // reference, alpha still builds.
    let beta_hash = &all_build.targets[0].input_hash;
    let existing = format!("registry.example.com/demo/beta:{beta_hash}");
    struct Has(String);
    impl RegistryProbe for Has {
        fn exists(&self, image_ref: &str) -> supercargo_core::Result<bool> {
            Ok(image_ref == self.0)
        }
    }
    let ws = repo.workspace();
    let vcs = repo.vcs();
    let plan = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &Has(existing.clone()),
        },
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(decisions(&plan), [("beta", false), ("svc-alpha", true)]);
    assert_eq!(
        plan.targets[0].decision,
        Decision::Skip {
            existing: Some(existing)
        }
    );
}

#[test]
fn hash_mode_refuses_dirty_tree() {
    let repo = DemoRepo::new();
    repo.append("crates/common/src/lib.rs", "// uncommitted\n");

    let ws = repo.workspace();
    let vcs = repo.vcs();
    let err = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &PlanOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, supercargo_core::Error::DirtyWorkTree(_)));

    let opts = PlanOptions {
        allow_dirty: true,
        ..Default::default()
    };
    strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &opts,
    )
    .unwrap();
}

#[test]
fn force_all_overrides_decisions() {
    let repo = DemoRepo::new();
    let base = repo.head();
    let ws = repo.workspace();
    let vcs = repo.vcs();
    let opts = PlanOptions {
        force_all: true,
        ..Default::default()
    };
    let plan = strategy::plan(&ws, &vcs, &Strategy::Diff { base }, &opts).unwrap();
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", true)]);
}

#[test]
fn many_changes_truncate_reasons() {
    let repo = DemoRepo::new();
    let base = repo.head();
    for i in 0..12 {
        repo.write(&format!("crates/svc-alpha/src/extra_{i}.rs"), "// filler\n");
    }
    repo.commit("feat: add many files");

    let plan = repo.diff_plan(&base);
    let alpha = &plan.targets[1];
    assert!(matches!(alpha.decision, Decision::Build));
    assert!(alpha.reasons.len() <= 9, "reasons: {:?}", alpha.reasons);
    assert!(
        alpha.reasons.last().unwrap().contains("more"),
        "reasons: {:?}",
        alpha.reasons
    );
}

#[test]
fn branch_and_latest_tags_follow_config() {
    let repo = DemoRepo::new();
    repo.replace(
        "Cargo.toml",
        "tag_with = [\"hash\", \"git-sha\"]",
        "tag_with = [\"hash\", \"branch\", \"latest\"]",
    );
    repo.commit("chore: tag with branch and latest");

    let ws = repo.workspace();
    let vcs = repo.vcs();
    let opts = PlanOptions {
        branch: Some("feat/new-tags".into()),
        ..Default::default()
    };
    let plan = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &opts,
    )
    .unwrap();
    let tags = &plan.targets[0].tags;
    assert_eq!(tags.len(), 3, "tags: {tags:?}");
    assert!(tags[0].starts_with("h1-"));
    assert_eq!(tags[1], "feat-new-tags");
    assert_eq!(tags[2], "latest");

    // Without a branch name the branch tag is simply absent.
    let plan = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.targets[0].tags.len(), 2);
}

#[test]
fn unknown_images_are_errors() {
    let repo = DemoRepo::new();
    let ws = repo.workspace();
    assert!(matches!(
        ws.image("nope"),
        Err(stevedore_err::UnknownImage(_))
    ));

    let vcs = repo.vcs();
    let opts = PlanOptions {
        only: vec!["nope".into()],
        ..Default::default()
    };
    let err = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &opts,
    )
    .unwrap_err();
    assert!(matches!(err, stevedore_err::UnknownImage(_)));
}

#[test]
fn only_filters_the_plan() {
    let repo = DemoRepo::new();
    let ws = repo.workspace();
    let vcs = repo.vcs();
    let opts = PlanOptions {
        only: vec!["beta".into()],
        ..Default::default()
    };
    let plan = strategy::plan(
        &ws,
        &vcs,
        &Strategy::Hash {
            probe: &NeverExists,
        },
        &opts,
    )
    .unwrap();
    assert_eq!(plan.targets.len(), 1);
    assert_eq!(plan.targets[0].name, "beta");
}

#[test]
fn metadata_without_a_binary_is_a_config_error() {
    let repo = DemoRepo::new();
    repo.append(
        "crates/dev-helper/Cargo.toml",
        "\n[package.metadata.supercargo]\n",
    );
    let err = repo.workspace().images().unwrap_err();
    match err {
        stevedore_err::Config { message, .. } => {
            assert!(message.contains("no [[bin]]"), "message: {message}")
        }
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[test]
fn unknown_metadata_fields_are_config_errors() {
    let repo = DemoRepo::new();
    repo.append(
        "crates/dev-helper/Cargo.toml",
        "\n[package.metadata.supercargo]\nbogus_field = true\n",
    );
    assert!(matches!(
        repo.workspace().images(),
        Err(stevedore_err::Config { .. })
    ));
}

#[test]
fn nested_workspaces_hash_and_plan() {
    // The cargo workspace lives in a subdirectory of the git repo; paths must
    // translate across the prefix in both directions.
    let repo = DemoRepo::new_nested("services/monorepo");
    let base = repo.head();
    repo.append("services/monorepo/crates/common/src/lib.rs", "// touched\n");
    repo.commit("feat: touch common");

    let plan = repo.diff_plan(&base);
    assert_eq!(decisions(&plan), [("beta", true), ("svc-alpha", true)]);
    assert!(plan.targets[0].input_hash.starts_with("h1-"));
}

/// (name, builds?) per target, in plan order.
fn decisions(plan: &BuildPlan) -> Vec<(&str, bool)> {
    plan.targets
        .iter()
        .map(|t| (t.name.as_str(), matches!(t.decision, Decision::Build)))
        .collect()
}

/// The `either` version the committed fixture lockfile pins.
const EITHER_VERSION: &str = "1.19.0";

/// A throwaway git repo seeded with the demo fixture.
struct DemoRepo {
    dir: tempfile::TempDir,
    workspace_subdir: String,
}

impl DemoRepo {
    fn new() -> Self {
        Self::new_at(".")
    }

    /// Seed the fixture at `subdir` inside the git repo ("." = repo root).
    fn new_at(subdir: &str) -> Self {
        let fixture = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/demo");
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(subdir);
        std::fs::create_dir_all(&target).unwrap();
        copy_tree(fixture.as_std_path(), &target);
        let repo = Self {
            dir,
            workspace_subdir: subdir.to_owned(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["add", "."]);
        repo.commit("chore: seed demo");
        repo
    }

    fn new_nested(subdir: &str) -> Self {
        Self::new_at(subdir)
    }

    fn root(&self) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.dir.path().to_path_buf()).unwrap()
    }

    fn workspace(&self) -> Workspace {
        let manifest = self.root().join(&self.workspace_subdir).join("Cargo.toml");
        Workspace::load(Some(&manifest)).unwrap()
    }

    fn vcs(&self) -> GitCli {
        GitCli::discover(self.workspace().root()).unwrap()
    }

    fn diff_plan(&self, base: &str) -> BuildPlan {
        let ws = self.workspace();
        let vcs = self.vcs();
        strategy::plan(
            &ws,
            &vcs,
            &Strategy::Diff {
                base: base.to_owned(),
            },
            &PlanOptions::default(),
        )
        .unwrap()
    }

    fn hash_plan(&self) -> BuildPlan {
        let ws = self.workspace();
        let vcs = self.vcs();
        strategy::plan(
            &ws,
            &vcs,
            &Strategy::Hash {
                probe: &NeverExists,
            },
            &PlanOptions::default(),
        )
        .unwrap()
    }

    fn head(&self) -> String {
        self.git_out(&["rev-parse", "HEAD"])
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root().join(path)).unwrap()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.root().join(path), contents).unwrap();
    }

    fn append(&self, path: &str, suffix: &str) {
        let mut text = self.read(path);
        text.push_str(suffix);
        self.write(path, &text);
    }

    fn replace(&self, path: &str, from: &str, to: &str) {
        let text = self.read(path);
        assert!(text.contains(from), "{path} does not contain {from:?}");
        self.write(path, &text.replace(from, to));
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "."]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn git(&self, args: &[&str]) {
        self.git_out(args);
    }

    fn git_out(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == ".git" || name == ".DS_Store" {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}
