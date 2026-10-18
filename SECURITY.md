# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately, not as a public issue. Use
GitHub's private vulnerability reporting for this repository (the "Security" tab
-> "Report a vulnerability"). Include a description, the affected version, and
reproduction steps. We aim to acknowledge within a few days and will keep you
posted on remediation.

## Scope

supercargo is a CI planning tool: no `unsafe` in our code, no network access of
its own, no credential handling. It reads cargo metadata and git state, and
shells out to `git` and (optionally, for registry probes) to `crane`, `skopeo`,
or `docker`, which use whatever authentication the environment already has. Its
security-relevant promise is that a build plan faithfully reflects the declared
inputs. In scope, for example:

- A path where a changed input yields a `skip` decision (a stale image is
  treated as current), including input-hash collisions reachable in practice.
- Command injection through configuration values (image names, tags, build
  args, paths) interpolated into the emitted CI configuration or into the
  commands supercargo spawns.
- A crafted repository or manifest that makes supercargo execute unexpected
  commands.

Documented behavior is not a vulnerability on its own: hash mode trusting the
registry's contents for its memoization, and the conservative whole-crate-dir
input granularity, are both by design and covered in the README.

## Supported versions

Pre-1.0: fixes land on the latest release published to crates.io.
