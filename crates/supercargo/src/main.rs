//! `supercargo`: compute which Docker images a Rust monorepo change
//! actually requires rebuilding, and emit CI configuration for the result.

use std::collections::BTreeSet;
use std::io::Read as _;

use anyhow::{Context, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand, ValueEnum};
use supercargo_core::emit::gitlab;
use supercargo_core::hash::WsPrefix;
use supercargo_core::plan::BuildPlan;
use supercargo_core::registry::{CommandProbe, HttpProbe, NeverExists, RegistryProbe};
use supercargo_core::strategy::{self, PlanOptions, Strategy};
use supercargo_core::vcs::{GitCli, Vcs};
use supercargo_core::workspace::Workspace;
use supercargo_core::{emit, inputs};

#[derive(Parser)]
#[command(name = "supercargo", bin_name = "supercargo", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compute the build plan and print it as JSON.
    Plan {
        #[command(flatten)]
        args: PlanArgs,
        /// Write the plan here instead of stdout.
        #[arg(long)]
        out: Option<Utf8PathBuf>,
    },
    /// Render a plan for a CI system.
    Emit {
        #[command(subcommand)]
        format: EmitFormat,
    },
    /// Show why one image would or would not rebuild.
    Explain {
        /// Image (or crate) name.
        name: String,
        #[command(flatten)]
        args: PlanArgs,
    },
    /// Validate the supercargo configuration and report discovered images.
    Check {
        /// Path to the workspace Cargo.toml (or its directory).
        #[arg(long)]
        manifest_path: Option<Utf8PathBuf>,
    },
    /// Show the input set and input hash of one image, for debugging
    /// determinism.
    Hash {
        /// Image (or crate) name.
        name: String,
        /// Also list every file entry that feeds the hash.
        #[arg(short, long)]
        verbose: bool,
        #[command(flatten)]
        args: PlanArgs,
    },
}

#[derive(Subcommand)]
enum EmitFormat {
    /// `$GITHUB_OUTPUT` lines: `matrix`, `has_work`, `count`, `skipped`.
    /// Append to the output file: `... emit github-matrix >> "$GITHUB_OUTPUT"`.
    GithubMatrix {
        #[command(flatten)]
        source: PlanSource,
    },
    /// A GitLab child pipeline. Jobs extend templates the repo defines (see
    /// --template-include); trigger it with `include: artifact:`.
    GitlabCi {
        #[command(flatten)]
        source: PlanSource,
        /// Repo-local YAML file defining the .supercargo-build / .supercargo-retag
        /// templates, included into the child pipeline.
        #[arg(long, default_value = ".gitlab/supercargo.yml")]
        template_include: String,
        /// Stage name for the emitted jobs.
        #[arg(long, default_value = "build")]
        stage: String,
        /// Hidden job the emitted build jobs extend.
        #[arg(long, default_value = ".supercargo-build")]
        build_template: String,
        /// Hidden job the emitted retag jobs extend (hash-mode skips that
        /// carry an existing reference).
        #[arg(long, default_value = ".supercargo-retag")]
        retag_template: String,
    },
}

/// Where an emit command gets its plan: a previously computed file ('-' for
/// stdin), or computed in-process from the same flags `plan` takes.
#[derive(Args)]
struct PlanSource {
    /// Read the plan JSON from this file ('-' = stdin) instead of computing.
    #[arg(long)]
    plan: Option<String>,
    #[command(flatten)]
    args: PlanArgs,
}

#[derive(Args)]
struct PlanArgs {
    /// Path to the workspace Cargo.toml (or its directory).
    #[arg(long)]
    manifest_path: Option<Utf8PathBuf>,

    /// Planning strategy. `auto` picks diff in a PR/MR context and hash
    /// otherwise.
    #[arg(long, value_enum, default_value_t = StrategyArg::Auto)]
    strategy: StrategyArg,

    /// Diff base ref. Defaults from the CI environment: origin/$GITHUB_BASE_REF
    /// or origin/$CI_MERGE_REQUEST_TARGET_BRANCH_NAME.
    #[arg(long)]
    base: Option<String>,

    /// Treat --base as the merge base itself (e.g. GitLab's
    /// $CI_MERGE_REQUEST_DIFF_BASE_SHA) instead of resolving one, which can
    /// fail on shallow clones.
    #[arg(long, requires = "base")]
    base_is_merge_base: bool,

    /// Registry probe for hash mode.
    #[arg(long, value_enum, default_value_t = ProbeArg::Http)]
    probe: ProbeArg,

    /// Restrict to these images (repeatable).
    #[arg(long = "only")]
    only: Vec<String>,

    /// Mark every image for rebuild regardless of strategy outcome.
    #[arg(long)]
    force_all: bool,

    /// Allow hashing HEAD from a dirty working tree.
    #[arg(long)]
    allow_dirty: bool,

    /// Branch name for the `branch` tag kind. Defaults from the CI
    /// environment.
    #[arg(long)]
    branch: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StrategyArg {
    Auto,
    Diff,
    Hash,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProbeArg {
    /// Registry HTTP API v2, with credentials from the standard docker and
    /// podman auth files (credential helpers included). No extra tool needed.
    Http,
    Crane,
    Skopeo,
    Docker,
    Podman,
    /// No probe: every image builds. Hash tags still memoize docker layer
    /// caches, but nothing is skipped.
    None,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Plan { args, out } => {
            let plan = compute_plan(&args)?;
            let json = emit::plan_json(&plan);
            match out {
                Some(path) => std::fs::write(&path, json).context("writing plan")?,
                None => print!("{json}"),
            }
        }
        Command::Emit { format } => match format {
            EmitFormat::GithubMatrix { source } => {
                let plan = load_or_compute(&source)?;
                print!("{}", emit::github::github_output(&plan));
            }
            EmitFormat::GitlabCi {
                source,
                template_include,
                stage,
                build_template,
                retag_template,
            } => {
                let plan = load_or_compute(&source)?;
                let opts = gitlab::Options {
                    template_include,
                    stage,
                    build_template,
                    retag_template,
                };
                print!("{}", gitlab::child_pipeline(&plan, &opts));
            }
        },
        Command::Explain { name, mut args } => {
            args.only = vec![name.clone()];
            explain(&name, &args)?;
        }
        Command::Check { manifest_path } => check(manifest_path.as_deref())?,
        Command::Hash {
            name,
            verbose,
            mut args,
        } => {
            args.only = vec![name.clone()];
            show_hash(&name, verbose, &args)?;
        }
    }
    Ok(())
}

fn load_workspace(manifest_path: Option<&Utf8Path>) -> anyhow::Result<(Workspace, GitCli)> {
    let ws = Workspace::load(manifest_path).context("loading cargo workspace")?;
    let vcs = GitCli::discover(ws.root()).context("discovering git repository")?;
    Ok((ws, vcs))
}

fn compute_plan(args: &PlanArgs) -> anyhow::Result<BuildPlan> {
    let (ws, vcs) = load_workspace(args.manifest_path.as_deref())?;
    let opts = PlanOptions {
        only: args.only.clone(),
        force_all: args.force_all,
        allow_dirty: args.allow_dirty,
        branch: args.branch.clone().or_else(branch_from_env),
    };

    let strategy = match args.strategy {
        StrategyArg::Diff => StrategyArg::Diff,
        StrategyArg::Hash => StrategyArg::Hash,
        StrategyArg::Auto => {
            if in_pr_context() {
                StrategyArg::Diff
            } else if std::env::var_os("CI").is_some() {
                StrategyArg::Hash
            } else {
                bail!(
                    "cannot auto-pick a strategy outside CI; pass --strategy diff --base <ref> \
                     or --strategy hash"
                );
            }
        }
    };

    let plan = match strategy {
        StrategyArg::Diff => {
            let base = args.base.clone().or_else(base_from_env).context(
                "no diff base: pass --base <ref> (merge-base with HEAD is used), or run in a \
                 PR/MR context where it is inferred",
            )?;
            strategy::plan(
                &ws,
                &vcs,
                &Strategy::Diff {
                    base,
                    base_is_merge_base: args.base_is_merge_base,
                },
                &opts,
            )?
        }
        StrategyArg::Hash => {
            let probe: Box<dyn RegistryProbe> = match args.probe {
                ProbeArg::Http => Box::new(HttpProbe::from_env()),
                ProbeArg::Crane => Box::new(CommandProbe::crane()),
                ProbeArg::Skopeo => Box::new(CommandProbe::skopeo()),
                ProbeArg::Docker => Box::new(CommandProbe::docker()),
                ProbeArg::Podman => Box::new(CommandProbe::podman()),
                ProbeArg::None => Box::new(NeverExists),
            };
            strategy::plan(
                &ws,
                &vcs,
                &Strategy::Hash {
                    probe: probe.as_ref(),
                },
                &opts,
            )?
        }
        StrategyArg::Auto => unreachable!("resolved above"),
    };
    Ok(plan)
}

fn load_or_compute(source: &PlanSource) -> anyhow::Result<BuildPlan> {
    let Some(from) = &source.plan else {
        return compute_plan(&source.args);
    };
    let text = if from == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading plan from stdin")?;
        buf
    } else {
        std::fs::read_to_string(from).with_context(|| format!("reading plan from {from}"))?
    };
    let plan: BuildPlan = serde_json::from_str(&text).context("parsing plan JSON")?;
    if plan.schema_version != supercargo_core::plan::PLAN_SCHEMA_VERSION {
        bail!(
            "plan schema version {} does not match this binary's {}",
            plan.schema_version,
            supercargo_core::plan::PLAN_SCHEMA_VERSION
        );
    }
    Ok(plan)
}

fn explain(name: &str, args: &PlanArgs) -> anyhow::Result<()> {
    let (ws, _) = load_workspace(args.manifest_path.as_deref())?;
    let target = ws.image(name)?;
    let set = inputs::resolve(&ws, &target)?;
    let plan = compute_plan(args)?;
    let planned = plan
        .targets
        .first()
        .context("image missing from its own plan")?;

    println!("image {} (crate {})", planned.image, planned.crate_name);
    println!("  decision: {}", decision_word(planned));
    for reason in &planned.reasons {
        println!("    - {reason}");
    }
    println!("  input hash: {}", planned.input_hash);
    println!("  tags: {}", planned.tags.join(", "));
    println!("  dockerfile: {}", planned.dockerfile);
    println!("  workspace crates in closure:");
    for (dir, krate) in &set.closure.members {
        println!("    - {krate} ({dir})");
    }
    println!(
        "  external packages in closure: {}",
        count_pins(&set.external_packages)
    );
    println!("  input paths:");
    for path in &set.paths {
        println!("    - {path}");
    }
    for ex in &set.excludes {
        println!("    - (excluded) {ex}");
    }
    Ok(())
}

fn show_hash(name: &str, verbose: bool, args: &PlanArgs) -> anyhow::Result<()> {
    let (ws, vcs) = load_workspace(args.manifest_path.as_deref())?;
    let target = ws.image(name)?;
    let set = inputs::resolve(&ws, &target)?;
    let prefix = WsPrefix::new(ws.root(), vcs.repo_root())?;
    if !args.allow_dirty {
        supercargo_core::hash::verify_clean(&set, &vcs, &prefix)?;
    }
    let head = vcs.resolve("HEAD")?;

    if verbose {
        println!("manifest fingerprint: {}", set.manifest_fingerprint);
        let paths: Vec<Utf8PathBuf> = set.paths.iter().map(|p| prefix.to_repo(p)).collect();
        for entry in vcs.tree_entries(&head, &paths)? {
            let Some(ws_path) = prefix.to_workspace(&entry.path) else {
                continue;
            };
            let marker = if set.is_excluded(&ws_path) {
                " (excluded)"
            } else {
                ""
            };
            println!("file: {} {} {}{marker}", entry.mode, entry.oid, ws_path);
        }
        for (pkg, versions) in &set.external_packages {
            for v in versions {
                println!("package: {pkg}@{v}");
            }
        }
        for (k, v) in &set.build_args {
            println!("build-arg: {k}={v}");
        }
    }
    println!(
        "{}",
        supercargo_core::hash::input_hash(&set, &vcs, &head, &prefix)?
    );
    Ok(())
}

fn check(manifest_path: Option<&Utf8Path>) -> anyhow::Result<()> {
    let (ws, _) = load_workspace(manifest_path)?;
    let images = ws.images()?;
    let mut problems = Vec::new();

    for target in &images {
        let set = inputs::resolve(&ws, target)?;
        println!(
            "image {}: crate {}, {} workspace crate(s), {} external package(s)",
            target.name,
            target.crate_name,
            set.closure.members.len(),
            set.external_packages.len(),
        );
        for path in std::iter::once(&target.dockerfile)
            .chain(&target.config.extra_paths)
            .chain(std::iter::once(&target.context))
        {
            if !ws.root().join(path).exists() {
                problems.push(format!(
                    "image {}: input path {path} does not exist",
                    target.name
                ));
            }
        }
    }

    // Binary crates without a supercargo table are legal (not every binary is
    // an image) but worth surfacing: a forgotten opt-in silently never builds.
    let image_crates: BTreeSet<&str> = images.iter().map(|t| t.crate_name.as_str()).collect();
    for name in ws.binary_crates() {
        if !image_crates.contains(name.as_str()) {
            println!("note: binary crate {name} has no [package.metadata.supercargo] table");
        }
    }

    if images.is_empty() {
        problems
            .push("no images: no workspace member carries [package.metadata.supercargo]".into());
    }
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("error: {p}");
        }
        bail!("{} problem(s) found", problems.len());
    }
    println!("ok: {} image(s)", images.len());
    Ok(())
}

fn decision_word(target: &supercargo_core::plan::Target) -> &'static str {
    match target.decision {
        supercargo_core::plan::Decision::Build => "build",
        supercargo_core::plan::Decision::Skip { .. } => "skip",
    }
}

fn count_pins(packages: &std::collections::BTreeMap<String, BTreeSet<String>>) -> usize {
    packages.values().map(BTreeSet::len).sum()
}

fn in_pr_context() -> bool {
    std::env::var("GITHUB_EVENT_NAME").is_ok_and(|e| e.starts_with("pull_request"))
        || std::env::var_os("CI_MERGE_REQUEST_IID").is_some()
}

fn base_from_env() -> Option<String> {
    std::env::var("GITHUB_BASE_REF")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("CI_MERGE_REQUEST_TARGET_BRANCH_NAME")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .map(|branch| format!("origin/{branch}"))
}

fn branch_from_env() -> Option<String> {
    for var in ["GITHUB_HEAD_REF", "GITHUB_REF_NAME", "CI_COMMIT_REF_NAME"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}
