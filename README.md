# codex-devices

A small Rust command-line tool for investigating cluttered Codex computer lists:
old names, retired registrations, duplicate labels, and identities copied during
machine migration. The project develops a reviewed audit, backup, rename,
cleanup, and verification workflow for one person with several computers.

**Development preview:** catalog mutations use invented data and change a local
simulation. Explicit operator captures can produce offline name-only request
proposals. Optional macOS diagnostics read existing sign-in metadata. Live
registry inventory, live rename, and live cleanup are not implemented. This
project is independent of OpenAI.

## The problem

A computer list can retain old registrations after migration or retirement.
Several entries can have the same name, and a healthy computer can retain an
outdated network name. A label alone cannot identify the record to change.
Changing an installation ID to fix a cosmetic name can create a separate
registration instead of repairing the existing one.

The screenshots show the intended outcome using fictitious machines:

| Before | After the synthetic workflow |
| --- | --- |
| ![Synthetic list with eleven personal registrations, duplicate names, and nine offline entries.](docs/images/devices-before.jpg) | ![Synthetic list after nine reviewed removals and a laptop rename, with both connected identities preserved.](docs/images/devices-after.jpg) |

These are screenshots of a demonstration rendered from synthetic CLI results.
They contain no live account data and are not screenshots of the Codex app.
The after state demonstrates the local simulation, not a verified live repair.

## Install

You need Git and the Rust toolchain with Cargo. Install Rust from
[rustup](https://rustup.rs/) if needed. The manifest requires Rust 1.89 or newer;
use a current stable toolchain. macOS is the first supported development target.

Install from this repository:

```sh
cargo install --git \
  https://github.com/bobbypierce42/codex-devices \
  --branch main --locked
codex-devices --version
```

Or install a local checkout:

```sh
git clone https://github.com/bobbypierce42/codex-devices.git
cd codex-devices
cargo install --path . --locked
codex-devices --version
```

Cargo places the executable in its installation root's `bin` directory, normally
`~/.cargo/bin`. Add that directory to your shell's `PATH` if the command is not
found. Successful installation prints a version beginning `codex-devices 0.1.0`.
The `--locked` option uses the committed dependency versions. No daemon,
database, interpreter, or background service is required. There is no crates.io,
Homebrew, or prebuilt binary release.

## Try the offline demo

Audit the bundled fixture without opening an account or contacting a computer:

```sh
codex-devices audit --demo
codex-devices audit --demo --json
```

Expect eleven personal registrations: two connected and nine offline, plus a
separate cloud record. Equal names remain separate records. Candidate counts
authorize no changes. Use `--input FILE` instead of `--demo` for an explicit
synthetic schema-v1 catalog with `demo-` identifiers, such as
[the bundled fixture](fixtures/synthetic-catalog.json).

### Back up and verify

Create an output directory, then save a verified backup in a new destination:

```sh
mkdir -p demo-output
codex-devices backup --demo --out demo-output/backup --json
codex-devices verify --backup demo-output/backup --json
```

Keep the printed manifest digest separately. To verify against that trusted
value, replace `MANIFEST_HASH` with the printed digest:

```sh
codex-devices verify --backup demo-output/backup \
  --expected-manifest-sha256 MANIFEST_HASH --json
```

The destination must be new and its parent must already exist. The backup
preserves exact input bytes and checks its files by SHA-256 and readback.
Without the separate digest, verification checks internal consistency only.
A metadata backup does not establish that deleted registrations can be restored.

### Review a rename

Prepare a synthetic laptop rename using its full environment ID:

```sh
codex-devices rename --demo \
  --set 'demo-active-laptop=Field laptop' \
  --out demo-output/rename --json
```

Preparation creates a verified backup and review plan without applying changes.
Read `demo-output/rename/plan.json`, check the selected ID and exact name, and
retain its printed `plan_sha256`. Replace `REVIEWED_HASH` with that digest:

```sh
codex-devices rename --apply demo-output/rename \
  --approve-plan-sha256 REVIEWED_HASH --json
codex-devices verify --operation demo-output/rename \
  --expected-plan-sha256 REVIEWED_HASH --json
```

Only the operation's private `service.json` changes. The original input, backup,
installation IDs, environment IDs, and access grants stay guarded. Names are
trimmed, limited to 100 UTF-16 code units, and reject control characters. The
demo's rename permission is invented and does not qualify a service API.

### Prepare a name-only request proposal

An explicit operator capture can produce a private proposal for one existing
registration. The file uses the same schema-v1 paginated envelope as the fixture,
with `source.kind` set to `operator-snapshot`. This label is a provenance statement,
not authentication. Known IDs must be unique; all pages must be present. This
command never retrieves a capture, reads identity files, or contacts a service:

```sh
codex-devices rename --snapshot capture.json \
  --set 'FULL_ENVIRONMENT_ID=Field laptop' --out demo-output/name-proposal --json
codex-devices verify --name-proposal demo-output/name-proposal \
  --expected-plan-sha256 REVIEWED_HASH --json
```

Preparation saves exact `before.json` bytes and an anchored `plan.json` with the
existing installation ID and proposed custom name. The receipt exposes only the
installation fingerprint. The observed internal request contract is unqualified;
there is no live apply transport. Both commands return `4`, with
`apply_supported=false` and `live_state_verified=false`.

Add `--input after-capture.json` to verification to compare an explicit later
capture. Only the selected display name may differ; all other metadata, records,
IDs, access fields, and array order must remain unchanged. Matching supplied
captures does not prove live service state. Keep captures and proposals private.

### Review a cleanup

Prepare removal of two specific offline fixture records:

```sh
codex-devices cleanup --demo \
  --select demo-retired-01 --select demo-retired-02 \
  --out demo-output/cleanup --json
```

Review the plan and its automatic backup. Replace `REVIEWED_HASH` with this
cleanup plan's digest, then apply and verify:

```sh
codex-devices cleanup --apply demo-output/cleanup \
  --approve-plan-sha256 REVIEWED_HASH --json
codex-devices verify --operation demo-output/cleanup \
  --expected-plan-sha256 REVIEWED_HASH --json
```

Offline status alone never authorizes removal. Selection requires full IDs;
connected and unselected records remain protected. Each simulated action has
an intent, response, and readback in a locked operation journal.

If a rename or cleanup stops partway through, retain its directory and inspect
the reported reason. Replace `--apply` with `--resume`, using the same operation
path and approved digest. Resume reconciles existing results before another
request. Never erase a journal or lock file to force progress.

## Check a Mac's sign-in metadata

This opt-in diagnostic starts a temporary child of the installed desktop app's
bundled app server. It requests no tokens, forced refresh, sign-in/sign-out,
computer inventory, or registration changes:

```sh
codex-devices audit --local --json
```

Local discovery checks fixed paths in `/Applications/ChatGPT.app` and
`/Applications/Codex.app`. If both exist or your app is elsewhere, select its
bundled executable with `--app-server ABSOLUTE_EXECUTABLE`. The tool does not
search credential files or installation IDs. The app server controls its own
normal startup and logging.

For an explicitly selected Mac reachable through existing SSH or a tailnet,
replace `SSH_ALIAS` and `REMOTE_EXECUTABLE` with your trusted host alias and the
absolute path of its installed app-server executable:

```sh
codex-devices audit --peer SSH_ALIAS \
  --app-server REMOTE_EXECUTABLE --json
```

The remote mode needs no installation of codex-devices on the peer. It uses
system SSH, existing authentication, strict host-key checks, and batch mode.
It disables forwarding, local commands, key updates, and connection sharing.
User-configured SSH proxies remain trusted configuration. There is no host
enrollment, network scan, automatic app shutdown, or remote service installation.
Remote child termination is not guaranteed after a broken connection.

Output contains allowlisted mode/support metadata and target provenance, without
email, account IDs, credentials, or server messages. Responses have a 15-second
deadline, 64 KiB pending-buffer cap, and 1 MiB total cap. Unsupported methods,
unexpected server requests, or unexpected returned tokens stop the check.
Missing support or a non-ChatGPT mode returns partial/unknown. A successful
result proves metadata compatibility, not account identity or registry access.
See the [official app-server authentication documentation](https://learn.chatgpt.com/docs/app-server#auth-endpoints).

## Support and limits

The development scope is:

| Area | Status |
| --- | --- |
| Synthetic workflows | Tested on macOS ARM64 |
| Local sign-in diagnostic | Observed with bundled CLI `0.166.0-alpha.5.1` on one Mac |
| SSH diagnostic | Mock-tested; real peer execution remains unqualified |
| Name-only request proposals | Offline operator captures; live transport and capability unqualified |
| Linux and Windows | Not qualified; Windows protected artifact operations refuse |
| Live registry editing, identity repair, plugin | Not implemented |

Input, encoded artifacts, journals, and JSON receipts are bounded to 4 MiB;
catalogs are limited to 4096 records. Private artifacts use owner/mode checks,
directory-relative file access, process locks, and conservative drift checks.
Inherited ACLs, hostile same-user races, and power-loss recovery remain limits.
Use trusted local storage. Verification does not query live computers. These
bounds do not define a total process memory or execution-time guarantee.

Exit codes are `0` success/preview, `1` I/O failure, `2` usage, `3` refusal,
`4` partial/unknown, and `5` integrity failure. JSON goes to stdout; failures go
to stderr. Large receipts can omit repeated review details while retaining all
outcomes, IDs, and digests; the receipt points back to their saved plan.

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and pull requests.
Any editor or agent runtime is welcome. Use invented fixtures, keep changes
focused, and report security concerns through [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE). Dependency licenses and notices remain their respective owners'.
