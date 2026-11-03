# Project guidance

Use any editor, agent, or runtime. Read README.md and CONTRIBUTING.md before
changing behavior. These instructions define project expectations, not a tool
installation or an orchestration workflow.

- **Think before coding.** State consequential assumptions and tradeoffs. Ask
  when ambiguity blocks a correct change; resolve routine choices directly.
- **Prefer simplicity.** Implement the requested behavior with the fewest
  concepts needed. Add abstractions only when they clarify real use cases.
- **Keep changes surgical.** Preserve unrelated work, existing contracts, and
  comments whose purpose you have not established. Avoid adjacent cleanup.
- **Verify the outcome.** Define an observable result, use focused behavioral
  checks, and report failed, skipped, or unverified work precisely.

Keep one Rust package and preserve `forbid(unsafe_code)`. Use invented data and
explicit temporary paths in development and tests. Do not read real credential
or installation-identity files, call account endpoints, or contact remote hosts.
Live acceptance requires a separately reviewed scope. Do not rotate an
installation ID to repair a name. Dependency, authentication, destructive-action,
and workflow-permission changes require maintainer review.

Before handoff, inspect the diff and run the relevant checks:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

The four principles above are independently worded from the community
[Karpathy-inspired guidelines](https://github.com/forrestchang/andrej-karpathy-skills),
which cite [Andrej Karpathy's observations](https://x.com/karpathy/status/2015883857489522876).
They do not imply endorsement by either author.
