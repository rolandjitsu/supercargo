# supercargo

[![CI](https://img.shields.io/github/actions/workflow/status/rolandjitsu/supercargo/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/rolandjitsu/supercargo/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/codecov/c/github/rolandjitsu/supercargo?style=flat-square)](https://codecov.io/gh/rolandjitsu/supercargo)
[![crates.io](https://img.shields.io/crates/v/supercargo?style=flat-square)](https://crates.io/crates/supercargo)
[![docs.rs](https://img.shields.io/docsrs/supercargo-core?style=flat-square)](https://docs.rs/supercargo-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)](./LICENSE)

Build only the Docker images that changed in a Rust monorepo.

A workspace with twenty binary crates does not need twenty image builds because
one crate changed - and it does not need zero builds hidden behind hand-written
path filters that miss a shared library bump. Supercargo walks the cargo
dependency graph, works out the exact input set of every image, and emits a
**build plan** your CI executes: GitHub Actions matrix, GitLab child pipeline,
or plain JSON for anything else.

## What counts as an input

For each image (a workspace binary crate that opts in), the plan considers:

- the crate's own sources, and every **transitive workspace dependency** -
  with dev-only edges excluded, so test helpers never trigger image builds;
- the **Cargo.lock subset** actually resolved for that binary: bumping one
  `[workspace.dependencies]` entry rebuilds only the images whose closure
  contains it, not the world;
- the root manifest, **normalized**: formatting and dependency-version edits
  are invisible, while `[profile]`/`[workspace.package]` changes correctly
  rebuild everything;
- the image's Dockerfile, declared `extra_paths` (configs, assets), and
  workspace-wide `global_paths` (shared base images, `rust-toolchain.toml`);
- minus declared `exclude_paths` (benches, docs).

File content enters through git blob OIDs, so hashing is fast, mtime-immune,
and identical on every machine.

## Two strategies

- **diff** (pull requests): compare against the merge-base of the target
  branch; rebuild what changed. No registry involved: nothing is probed and
  nothing has to be published.
- **hash** (main): compute a deterministic input hash per image and skip the
  build when `image:h1-<hash>` already exists - the registry is the
  memoization table. The contract: **your build job pushes the hash tag**
  (the example workflows do) - the next plan finds it and skips. Without that
  push, nothing is ever skipped. Red builds, reverts, and force pushes all
  self-heal: there is no "last green commit" bookkeeping to corrupt. Skipped
  images are retagged registry-side instead of rebuilt. Existence checks use
  the registry HTTP API with the credentials docker/podman already store
  (credential helpers included); `--probe crane|skopeo|docker|podman` shells
  out to a CLI instead, `--probe none` disables skipping.

## Quickstart

Opt crates in via Cargo metadata - no separate config file:

```toml
# workspace Cargo.toml
[workspace.metadata.supercargo]
registry = "ghcr.io/acme"
global_paths = ["docker/base.Dockerfile"]
tag_with = ["hash", "git-sha"]

# a binary crate's Cargo.toml
[package.metadata.supercargo]        # empty table = all defaults
extra_paths = ["configs/ingest.yaml"]
```

Then:

```shell
cargo install supercargo --locked

supercargo check                               # validate config
supercargo plan --strategy diff --base origin/main
supercargo explain my-image --strategy diff --base origin/main
```

`plan` prints versioned JSON; `explain` tells you *why* an image rebuilds:

```text
image ghcr.io/acme/ingest (crate svc-ingest)
  decision: build
    - crate common changed: crates/common/src/retry.rs
    - dependency serde changed in Cargo.lock
  input hash: h1-658df1883fcf721e2890da369af418ba
  ...
```

## CI integration

**GitHub Actions** - the composite action computes the plan; a matrix job
builds it (full example: [examples/github-build-images.yml](./examples/github-build-images.yml)):

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.supercargo.outputs.matrix }}
      has_work: ${{ steps.supercargo.outputs.has_work }}
    steps:
      - uses: actions/checkout@v7
        with: { fetch-depth: 0 }
      - uses: rolandjitsu/supercargo@v0.1.0
        id: supercargo
        with: { version: 0.1.0 }

  build:
    needs: plan
    if: needs.plan.outputs.has_work == 'true'
    strategy:
      matrix: ${{ fromJson(needs.plan.outputs.matrix) }}
    # docker/build-push-action with matrix.dockerfile, matrix.tags_csv, ...
```

**GitLab CI** - `emit gitlab-ci` generates a dynamic child pipeline whose jobs
`extends` templates your repo owns, so supercargo decides *what* to build and
your templates decide *how*. The plan job can run on the published tool image,
`ghcr.io/rolandjitsu/supercargo` (supercargo plus the cargo and git it needs),
instead of installing a binary (full example: [examples/gitlab-ci.yml](./examples/gitlab-ci.yml)):

```yaml
supercargo:plan:
  script:
    - supercargo plan --base "$CI_MERGE_REQUEST_DIFF_BASE_SHA" --base-is-merge-base --out plan.json
    - supercargo emit gitlab-ci --plan plan.json > child-pipeline.yml
  artifacts: { paths: [plan.json, child-pipeline.yml] }

supercargo:build:
  trigger:
    include: [{ artifact: child-pipeline.yml, job: "supercargo:plan" }]
    strategy: depend
```

**Anything else** - consume the plan JSON directly; the schema is versioned
(`schema_version`) and `jq`-friendly.

## Configuration

The model is **one image per opted-in binary crate**: every workspace member
that carries a `[package.metadata.supercargo]` table (even an empty one)
becomes exactly one image, named after the crate unless `image` overrides it.
A crate with several `[[bin]]` targets is still one image - its Dockerfile
decides what gets copied in. The workspace-level table never declares images;
it only holds the settings shared by all of them.

`[workspace.metadata.supercargo]`:

| key | default | meaning |
| --- | --- | --- |
| `registry` | none | prefix for image references |
| `global_paths` | `[]` | paths that invalidate every image (`rust-toolchain*` and `.cargo/config.toml` are always included) |
| `tag_with` | `["hash", "git-sha"]` | tag families: `hash`, `git-sha`, `branch`, `latest` |
| `default_dockerfile` | none | fallback for crates without their own Dockerfile |

`[package.metadata.supercargo]` (presence opts the binary crate in):

| key | default | meaning |
| --- | --- | --- |
| `image` | crate name | image name |
| `dockerfile` | `<crate>/Dockerfile` | Dockerfile, relative to the crate |
| `context` | workspace root | docker build context |
| `extra_crates` | `[]` | additional workspace crates the image bundles; their closures join the input set |
| `extra_paths` | `[]` | extra inputs (workspace-relative) |
| `exclude_paths` | `[]` | carve-outs (crate-relative) |
| `build_args` | `{}` | `--build-arg` values; part of the input hash |

## Guarantees and limits

- The input hash is a stability contract: its scheme version is embedded in
  the tag (`h1-...`) and bumped on any change to what is hashed, so a tool
  upgrade rebuilds everything once, visibly, instead of mismatching silently.
- Hashing reads committed git state and refuses a dirty tree (override with
  `--allow-dirty`).
- `build.rs` is undecidable in general: the whole crate directory counts by
  default, and inputs outside it must be declared via `extra_paths`.
- Dockerfiles are tracked inputs; what is not tracked is a `FROM` tag being
  re-pointed upstream with no change in your repo (`rust:1` resolving to a new
  digest). The recommended practice: pin digests
  (`FROM rust:1@sha256:...`) and let Renovate or dependabot bump them - a base
  move then becomes an ordinary Dockerfile change that both strategies see,
  review, and revert like any other input. A periodic `--force-all` rebuild is
  the fallback for unpinned bases.

## Prior art

[`determinator`](https://crates.io/crates/determinator) computes changed
workspace packages (supercargo adds the image layer: input hashing, registry
memoization, CI emitters). [`dorny/paths-filter`](https://github.com/dorny/paths-filter)
and GitLab `rules:changes` are the path-glob status quo this replaces.
[`cargo-chef`](https://crates.io/crates/cargo-chef) caches Docker *layers*
within one build and composes well with supercargo deciding *whether* that
build runs. `nx affected` and Turborepo prove the UX for JS monorepos.
Supercargo's graph queries run on [`guppy`](https://crates.io/crates/guppy).

## Demo

[`tests/fixtures/demo`](./tests/fixtures/demo) is a complete miniature
monorepo (two images, a shared library, a dev-only helper) used by the
integration tests; point the CLI at it to poke around:

```shell
supercargo check --manifest-path tests/fixtures/demo/Cargo.toml
```

## Status

Early (0.1.x): plan/emit/explain/check/hash subcommands, diff + hash
strategies, GitHub Actions + GitLab CI + JSON emitters. The plan schema and
hash scheme are versioned; breaking either bumps the respective version.

## License

[Apache-2.0](./LICENSE)
