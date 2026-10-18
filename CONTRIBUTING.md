# Contributing

Thanks for contributing to supercargo. These guidelines keep history clean and review fast.

## Ground rules

1. Commits follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
2. Keep commits small and self-contained so each can be reviewed on its own. Split large
   changes into incremental commits/PRs; never mix unrelated changes in one commit.
3. Every change ships with tests (unit, plus integration where it fits). Coverage should not drop.
4. CI must be green before review: formatting, clippy, and tests.
5. Address review feedback by amending the relevant commit and rebasing, not with follow-up
   `fix: typo` commits. Keep history linear.

## Commit messages

A Conventional Commits subject in the present tense, imperative voice (`feat: add gitlab
emitter`, not `feat: added` or `feat: adds`), then an imperative body that explains *why* when
it is not obvious, wrapped at ~72 columns, ASCII only (no em-dash, no `--`).

AI-assisted commits disclose the assistant and model with an `Assisted-by: <Assistant>:<model>`
trailer - whichever assistant and model apply - following the kernel
[coding-assistants guidance](https://docs.kernel.org/process/coding-assistants.html). Do not use
`Co-Authored-By`.

```
feat: exclude dev-only edges from image closures

A change to a test helper crate must not rebuild images; resolve the
dependency closure without dev-only links so it cannot.

Assisted-by: Claude:claude-fable-5
```

Agents have extra rules in [AGENTS.md](./AGENTS.md).

## Development

```shell
cargo build
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo llvm-cov --workspace --all-features --locked --summary-only   # coverage, matches CI
```

Snapshot tests use [insta](https://insta.rs); review changed snapshots with `cargo insta review`
(never hand-edit `.snap` files). The integration tests copy `tests/fixtures/demo` into throwaway
git repos, so a working `git` is required.

Install [prek](https://github.com/j178/prek) and enable the git hooks once. They run typos
and rustfmt on commit, a Conventional Commits check on the message, and clippy on push:

```shell
brew install prek   # or: cargo install --locked prek
prek install
```

## Rust style

Follow the [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/); `rustfmt` and
`clippy` are enforced in CI. Prefer idiomatic Rust. Keep comments concise and reserved for the
non-obvious; do not narrate the code. Optional: [typos](https://github.com/crate-ci/typos) for
spell checking.

## Design

The architecture and rationale live in the [README](./README.md) and the `supercargo-core`
rustdoc. The invariants that matter most: the plan JSON and the input hash are versioned public
contracts (`PLAN_SCHEMA_VERSION`, `HASH_SCHEME`); correctness errs toward rebuilding (never a
path where a changed input yields a skip); and supercargo plans builds but never performs them.
