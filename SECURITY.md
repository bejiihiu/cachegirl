# Security Policy

## Supported versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

This is a pre-1.0 crate; only the latest `0.1.x` release receives fixes.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public issues,
pull requests, discussions, or chat.**

cachegirl handles temporary files, directories, lock files, and process
metadata (including liveness checks), and it is designed to be safe against
hostile inputs such as path traversal and tampered on-disk state. If you find
something wrong here, report it privately:

- **GitHub Security Advisories**: go to the
  [security advisories page](https://github.com/bejiihiu/cachegirl/security/advisories/new)
  and open a private vulnerability report. This is the preferred channel —
  reports stay private until a fix is released.

Please include:

- affected version and feature flags,
- operating system and filesystem (e.g. NTFS, ext4),
- steps to reproduce or a proof of concept,
- the impact you believe the issue has.

## What happens next

- You will get an acknowledgment within a few days.
- The report will be triaged and a fix developed privately.
- Once a fix is available, a patch release is cut and the advisory is
  published, crediting the reporter (unless you ask to remain anonymous).

## Scope

In scope:

- path confinement and traversal issues in the filesystem backend,
- lock-file / recovery logic that could delete or corrupt unrelated files,
- resource accounting that can be driven into denial of service,
- anything where untrusted on-disk state (registry, lock files, metadata)
  leads to unintended file deletion, information disclosure, or privilege
  escalation.

Out of scope:

- vulnerabilities in dependencies already covered by `cargo audit` output
  (report those upstream),
- issues requiring a hostile local process with the same privileges as the
  user running the library (process isolation is not part of the threat
  model),
- denial of service through simply creating very large numbers of resources
  when quotas are deliberately set high or disabled,
- bugs in consumers' scheduler integrations.

## Dependency auditing

CI runs `cargo audit` against the lockfile on pushes to `main` and on a
weekly schedule. If you are packaging cachegirl yourself (vendored deps,
custom resolutions), you are responsible for auditing your own dependency
tree.
