# AGENTS.md

Guidance for AI coding agents in this repo. Human contributors: see [CONTRIBUTING.md](./CONTRIBUTING.md).

## Workflow

- Clarify the design before implementing. For anything non-trivial, agree on the approach first;
  prefer a short design note over jumping to code.
- One unit of change per commit. Never mix unrelated changes. Present the change for review
  before committing.
- Every change ships with tests. Run local CI before calling it done, and do not claim it passes
  without running it.
- Verify against the code and the tools: read before you answer, run before you assert.

Local CI:

```shell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Writing: code, comments, docs, commits

- Concise and to the point. No fluff. Explain the non-obvious; do not narrate the obvious.
- ASCII only. No em-dash and no `--`; write `-`. Write `->` for the right arrow, `!=` for
  not-equal, straight quotes for curly ones, and the same for every other non-ASCII glyph.
  Applies everywhere, including this file.
- Comments justify *why*, not *what*. Delete any comment that restates the code.
- Do not use the word "seam"; say boundary, interface, or extension point. Do not use "bespoke";
  say "custom". Do not use "mint"; say create or issue. Do not use "speaks" for code or
  protocols; say uses, takes, or expects. Code does not "ask questions"; it checks.
- Use "etc.", not "...", when a list trails off.
- Describe mechanics literally, not figuratively.

## Commits

- Conventional Commits (see CONTRIBUTING.md). Write the subject in the present tense, imperative
  voice: `feat: add gitlab emitter`, not `added` or `adds`.
- Keep the body minimal. The subject alone is often enough; add body lines only for the
  non-obvious *why*. Do not restate the diff or enumerate every file changed.
- Subject <= 50 characters. Wrap body lines at 72 columns and keep the body to a few lines. The
  `committed` hook and CI enforce both limits.
- A commit that addresses a tracked issue ends its body with `Closes #X.` (before the
  trailer), so merging to main closes the issue.
- Disclose AI assistance with an `Assisted-by: <Assistant>:<model>` trailer, whatever the
  assistant and model (e.g. `Assisted-by: Claude:claude-fable-5`). Never `Co-Authored-By`, and
  never add a human's `Signed-off-by`.

## Tests

- Unit tests inline (`#[cfg(test)] mod tests`); public-surface tests in `tests/`.
- Put helpers *after* the tests that use them.
- Coverage must not drop. Keep line coverage at or above its current level, and never below 90%.
  CI fails the build under 90%.
- Planner behavior is tested end-to-end against `tests/fixtures/demo` (repo root), copied into a
  throwaway git repo per test. Emitters are pure functions of a plan; test them with hand-built
  plans and insta snapshots, no git involved.
- The fixture commits a real Cargo.lock; tests must not depend on network beyond what a committed
  lockfile already resolved (the `either` crate is the one external dep, pinned).

## Terminology

- **image / target**: a workspace binary crate that opted in via
  `[package.metadata.supercargo]`, packaged as one Docker image.
- **closure**: the image's transitive dependency set with dev-only edges excluded - workspace
  members plus resolved external packages.
- **input set**: everything that determines the image's bytes: closure crate dirs, Dockerfile,
  extra paths, global paths, lockfile subset, build args, normalized root manifest.
- **input hash**: blake3 over the input set, fed by git blob OIDs, formatted
  `h<scheme>-<32 hex>`. The memoization key and immutable tag.
- **plan**: the versioned JSON (`plan::PLAN_SCHEMA_VERSION`) listing every target with a
  build/skip decision and human-readable reasons. The public interface; emitters are thin.
- **strategy**: how decisions are made - `diff` (changed since merge-base) or `hash`
  (input hash absent from the registry, probed over HTTP or via crane/skopeo/docker/podman).

## Code conventions

- No `unsafe` in our code.
- The plan JSON and the input hash are public contracts: any shape change bumps
  `PLAN_SCHEMA_VERSION`, any hashing change bumps `HASH_SCHEME`. Never change either silently.
- Decisions must carry reasons. A build or skip the user cannot `explain` is a bug.
- When unsure whether something is an input, count it (rebuild more); precision improvements
  must be provably safe. Never introduce a path where a changed input yields a skip.
- supercargo plans builds; it never performs them. No registry write calls in this codebase -
  probes are read-only existence checks (HTTP HEAD, or crane/skopeo/docker/podman).
- Paths in config and plans are workspace-relative (repo-relative only inside `vcs`); the
  `WsPrefix` type converts at that boundary.
- Document every public item with rustdoc; `#![warn(missing_docs)]` is on.

## CI workflows

- GitHub Actions live in `.github/workflows`. Write the workflow `name:`, every job name, and
  every named step in Sentence case, matching `ci.yml`.
- Keep workflows minimal and scoped to one purpose; prefer the built-in `GITHUB_TOKEN`
  (release.yml's app token is the one justified exception).
