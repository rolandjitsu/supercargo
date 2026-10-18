---
name: Bug Report
about: Report something that does not work as documented
title: "[BUG] "
labels: bug
assignees: ''

---

**Describe the bug**
A clear and concise description of what goes wrong.

**Minimal reproducible example**
The smallest setup that shows the problem - ideally a tiny workspace layout plus
the command and its output:

```shell
supercargo plan --strategy diff --base origin/main
# or: supercargo explain <image> ... / supercargo hash <image> -v ...
```

A wrong decision is best reported with the `explain` (or `hash -v`) output for
the affected image.

**Expected behavior**
What you expected to happen instead (e.g. which images should have been
rebuilt or skipped, and why).

**Environment**
- supercargo version (`supercargo --version`):
- CI system (GitHub Actions / GitLab CI / other) and strategy (diff / hash):
- rustc version (`rustc --version`):
- git version, OS / arch:

**Additional context**
Anything else that helps: the plan JSON, the relevant
`[package.metadata.supercargo]` / `[workspace.metadata.supercargo]` tables, and a
backtrace (`RUST_BACKTRACE=1`) for panics.
