//! CLI smoke tests: run the real binary against the demo fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn check_reports_images() {
    let repo = demo_repo();
    let out = supercargo(&["check", "--manifest-path", &manifest(&repo)]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("ok: 2 image(s)"), "stdout: {stdout}");
}

#[test]
fn plan_emits_valid_json_and_pipes_into_emit() {
    let repo = demo_repo();
    let base = git_out(repo.path(), &["rev-parse", "HEAD"]);
    append(repo.path(), "crates/common/src/lib.rs", "// touched\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feat: touch common"]);

    let out = supercargo(&[
        "plan",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "diff",
        "--base",
        &base,
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["targets"].as_array().unwrap().len(), 2);
    assert_eq!(plan["targets"][0]["decision"]["action"], "build");

    // Feed the plan file into both emitters.
    let plan_path = repo.path().join("plan.json");
    std::fs::write(&plan_path, &out.stdout).unwrap();
    let plan_arg = plan_path.to_str().unwrap();

    let gh = supercargo(&["emit", "github-matrix", "--plan", plan_arg]);
    let gh_text = String::from_utf8_lossy(&gh.stdout);
    assert!(gh.status.success());
    assert!(gh_text.contains("has_work=true"));
    assert!(gh_text.contains("count=2"));

    let gl = supercargo(&["emit", "gitlab-ci", "--plan", plan_arg]);
    assert!(gl.status.success());
    let pipeline: serde_json::Value = serde_json::from_slice(&gl.stdout).unwrap();
    assert!(pipeline.get("build:beta").is_some());
    assert!(pipeline.get("build:svc-alpha").is_some());
}

#[test]
fn explain_names_the_changed_crate() {
    let repo = demo_repo();
    let base = git_out(repo.path(), &["rev-parse", "HEAD"]);
    append(repo.path(), "crates/common/src/lib.rs", "// touched\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feat: touch common"]);

    let out = supercargo(&[
        "explain",
        "beta",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "diff",
        "--base",
        &base,
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("decision: build"), "stdout: {stdout}");
    assert!(stdout.contains("crate common changed"), "stdout: {stdout}");
}

#[test]
fn hash_subcommand_dumps_the_input_set() {
    let repo = demo_repo();
    let out = supercargo(&[
        "hash",
        "svc-alpha",
        "-v",
        "--manifest-path",
        &manifest(&repo),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("manifest fingerprint:"), "stdout: {stdout}");
    assert!(stdout.contains("file: "), "stdout: {stdout}");
    assert!(stdout.contains("package: either@"), "stdout: {stdout}");
    assert!(
        stdout.lines().last().unwrap().starts_with("h1-"),
        "stdout: {stdout}"
    );
}

#[test]
fn plan_writes_out_file_and_emit_reads_stdin() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let repo = demo_repo();
    let plan_path = repo.path().join("plan.json");
    let out = supercargo(&[
        "plan",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "hash",
        "--probe",
        "none",
        "--out",
        plan_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan_text = std::fs::read_to_string(&plan_path).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_supercargo"))
        .args(["emit", "github-matrix", "--plan", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(plan_text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("count=2"));
}

#[test]
fn emit_rejects_a_plan_from_a_different_schema() {
    let repo = demo_repo();
    let plan_path = repo.path().join("plan.json");
    std::fs::write(
        &plan_path,
        r#"{"schema_version": 99, "strategy": "diff", "head": "x", "targets": []}"#,
    )
    .unwrap();
    let out = supercargo(&[
        "emit",
        "github-matrix",
        "--plan",
        plan_path.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("schema version"));
}

#[test]
fn check_fails_when_a_dockerfile_is_missing() {
    let repo = demo_repo();
    std::fs::remove_file(repo.path().join("crates/svc-beta/Dockerfile")).unwrap();
    let out = supercargo(&["check", "--manifest-path", &manifest(&repo)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Dockerfile"));
}

#[test]
fn only_and_force_all_narrow_and_force_the_plan() {
    let repo = demo_repo();
    let out = supercargo(&[
        "plan",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "hash",
        "--probe",
        "none",
        "--only",
        "beta",
        "--force-all",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let targets = plan["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["name"], "beta");
    assert!(
        targets[0]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("forced")
    );

    let out = supercargo(&[
        "plan",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "hash",
        "--probe",
        "none",
        "--only",
        "nope",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no image named nope"));
}

#[test]
fn auto_strategy_follows_the_ci_context() {
    let repo = demo_repo();
    // PR context: auto resolves to diff, base inferred from GITHUB_BASE_REF
    // via a local branch standing in for the remote-tracking ref.
    git(repo.path(), &["branch", "origin/main"]);
    append(repo.path(), "crates/common/src/lib.rs", "// touched\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "feat: touch common"]);
    let out = supercargo_env(
        &["plan", "--manifest-path", &manifest(&repo)],
        &[
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_BASE_REF", "main"),
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["strategy"], "diff");

    // Non-PR CI: auto resolves to hash.
    let out = supercargo_env(
        &[
            "plan",
            "--manifest-path",
            &manifest(&repo),
            "--probe",
            "none",
        ],
        &[("CI", "true")],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["strategy"], "hash");

    // Outside CI, auto refuses to guess.
    let out = supercargo_env(&["plan", "--manifest-path", &manifest(&repo)], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot auto-pick"));
}

#[test]
fn gitlab_emit_produces_a_noop_pipeline_when_nothing_changed() {
    let repo = demo_repo();
    let head = git_out(repo.path(), &["rev-parse", "HEAD"]);
    let out = supercargo(&[
        "emit",
        "gitlab-ci",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "diff",
        "--base",
        &head,
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("supercargo:nothing-to-build"),
        "stdout: {stdout}"
    );
}

#[test]
fn explain_reports_skips_too() {
    let repo = demo_repo();
    let head = git_out(repo.path(), &["rev-parse", "HEAD"]);
    let out = supercargo(&[
        "explain",
        "beta",
        "--manifest-path",
        &manifest(&repo),
        "--strategy",
        "diff",
        "--base",
        &head,
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("decision: skip"), "stdout: {stdout}");
    assert!(stdout.contains("no inputs changed"), "stdout: {stdout}");
}

fn supercargo(args: &[&str]) -> Output {
    supercargo_env(args, &[])
}

/// Run the binary with the CI-detection variables cleared, then `set` applied,
/// so tests behave the same on a laptop and inside GitHub Actions.
fn supercargo_env(args: &[&str], set: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_supercargo"));
    for var in [
        "CI",
        "GITHUB_EVENT_NAME",
        "GITHUB_BASE_REF",
        "GITHUB_HEAD_REF",
        "GITHUB_REF_NAME",
        "CI_MERGE_REQUEST_IID",
        "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
        "CI_COMMIT_REF_NAME",
    ] {
        cmd.env_remove(var);
    }
    for (key, value) in set {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

fn manifest(repo: &tempfile::TempDir) -> String {
    repo.path().join("Cargo.toml").to_str().unwrap().to_owned()
}

fn demo_repo() -> tempfile::TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/demo");
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture, dir.path());
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "chore: seed demo"]);
    dir
}

fn append(root: &Path, path: &str, suffix: &str) {
    let file = root.join(path);
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str(suffix);
    std::fs::write(&file, text).unwrap();
}

fn git(cwd: &Path, args: &[&str]) {
    git_out(cwd, args);
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == ".git" || name == ".DS_Store" {
            continue;
        }
        let dest: PathBuf = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}
