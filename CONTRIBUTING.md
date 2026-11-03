# Contributing

Contributions are welcome through issues and pull requests. The project keeps
one Rust package and a small command-line interface. macOS comes first.

## Set up a development checkout

1. Install a current stable Rust toolchain and Git.
2. Fork the repository and clone your fork.
3. Create a descriptive topic branch in your fork, based on `main`.
4. Run the checks from the repository root:

   ```sh
   cargo fmt --check
   cargo clippy --locked --all-targets -- -D warnings
   cargo test --locked
   cargo build --locked --release
   ```

CI repeats these checks on stable Rust and Rust 1.89, and uses pinned cargo-audit
to check Cargo.lock against the RustSec advisory database. The Linux audit job
does not establish Linux support for the application.

Tests use synthetic data and explicit temporary paths. They must not access
real authentication stores, installation identities, accounts, or remote hosts.
No particular editor, model, agent, skill collection, or orchestration system
is required. [AGENTS.md](AGENTS.md) contains brief project guidance for agents.

## Propose a change

- For a bug, include the command, tool version, OS/architecture, expected result,
  and actual result. Reproduce it with invented data where possible.
- For a feature, describe the user problem and smallest useful change first.
- Keep unrelated refactors and dependency updates separate.
- Add a behavioral regression test for a substantive defect. Preserve
  conservative handling of unknown results and guarded installation identities.
- Never attach tokens, account/device identifiers, private hostnames, backups,
  raw session material, or personal screenshots to issues or pull requests.

## Open a pull request

Target `main` from your fork. Describe the problem, resulting behavior, and checks
you ran. Identify skipped checks and platform limits. CI must pass and a maintainer
must review changes before merging; the maintainer handles merges and releases.
Topic branches belong in contributor forks so the project can keep only `main`.

Authentication, account mutations, cryptography, filesystem protection, new
dependencies, and workflow permission changes need explicit maintainer review.
Do not run live experiments as part of a contribution. Discuss a separately
scoped acceptance procedure before any such test.

By submitting a contribution, you confirm you can provide it under the project's
[MIT license](LICENSE). Report suspected vulnerabilities privately as described
in [SECURITY.md](SECURITY.md).
