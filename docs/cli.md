# Command-line tools

Codex Devices includes two command-line tools:

- `codex-devices-mac` reads and changes a selected account label on macOS using
  an existing desktop sign-in.
- `codex-devices` runs offline simulations and optional sign-in diagnostics. Its
  simulated rename and cleanup commands do not change an account.

Both tools are development previews. The macOS account workflow uses an
unsupported internal API and has synthetic tests; live acceptance is incomplete.

## Build the tools

Follow the [README requirements](../README.md#requirements), then download and
open the repository as described in the first two [installation steps](../README.md#install).
To build the tools, run:

```sh
bash ./script/build_cli.sh
./dist/codex-devices-mac --help
./dist/codex-devices --help
```

Keep the two executables together in `dist`: the macOS tool uses the Rust tool
to prepare and verify its operations. Building and viewing help do not access an
account. Installing only the Rust package does not provide `codex-devices-mac`.

## Rename an account label

The examples in this section contact the signed-in account. Their interface is
covered by synthetic tests; these examples have not completed live acceptance.
Use the Mac with Codex.app or ChatGPT.app signed in to the intended account.
When both are installed, the tool prefers Codex.app. To select a different bundled
executable, pass its absolute path with `--app-server`.

1. Read the account's computer list:

   ```sh
   ./dist/codex-devices-mac audit
   ```

   The JSON output includes online and offline records. Identify the exact
   personal computer by its full environment ID. Equal labels can belong to
   different registrations. Keep the output private.

2. Create a private parent directory once, then prepare a rename in a new
   operation directory. Replace `FULL_ENVIRONMENT_ID` with the selected ID and
   `Field laptop` with the desired label:

   ```sh
   mkdir -m 700 "$HOME/.codex-devices-operations"
   ./dist/codex-devices-mac rename \
     --id FULL_ENVIRONMENT_ID --name 'Field laptop' \
     --out "$HOME/.codex-devices-operations/rename-01"
   ```

   Use a different, nonexistent operation directory for each review. Preparation
   saves a private backup and plan; it does not send a rename request.

3. Review the old and new labels, the full ID, and
   `rename-01/engine/plan.json`. Keep the printed `plan_sha256` and replace
   `REVIEWED_HASH` with that value when you are ready to apply:

   ```sh
   ./dist/codex-devices-mac apply \
     --operation "$HOME/.codex-devices-operations/rename-01" \
     --approve-plan-sha256 REVIEWED_HASH
   ```

4. To read the account state again without sending another rename, run:

   ```sh
   ./dist/codex-devices-mac verify \
     --operation "$HOME/.codex-devices-operations/rename-01" \
     --expected-plan-sha256 REVIEWED_HASH
   ```

The tool checks the response and fresh account readback, including preservation
of the other records. A missing or mismatched response remains uncertain even if
the label appears correct. Do not repeat the request; retain the entire operation
folder for investigation. The backup contains private metadata and does not
promise automatic rollback.

## Try the offline simulation

Audit the bundled invented data without opening an account or contacting a Mac:

```sh
./dist/codex-devices audit --demo
```

The output lists 11 personal registrations and one cloud record. It identifies
nine offline candidates for review, but does not authorize or perform removal.

To try a simulated rename, create a new output directory and prepare a plan:

```sh
mkdir -m 700 demo-output
./dist/codex-devices rename --demo \
  --set 'demo-active-laptop=Field laptop' \
  --out demo-output/rename --json
```

Review `demo-output/rename/plan.json` and keep the printed `plan_sha256`. Replace
`REVIEWED_HASH` with that digest to apply and verify the simulation:

```sh
./dist/codex-devices rename --apply demo-output/rename \
  --approve-plan-sha256 REVIEWED_HASH --json
./dist/codex-devices verify --operation demo-output/rename \
  --expected-plan-sha256 REVIEWED_HASH --json
```

The simulation changes only its private service copy and journal. It does not
change the original fixture or a real account. The Rust tool can also simulate
cleanup, create backups, and prepare offline proposals from an operator capture;
run `./dist/codex-devices --help` for those options. Snapshot proposals do not
have a live apply transport, and verification of supplied captures does not
prove live account state.

## Sign-in diagnostics

The Rust tool's `audit --local` and `audit --peer` options inspect sign-in mode
metadata. They do not list account computers or establish that two Macs use the
same account. Peer diagnostics require an explicit host, existing SSH access,
a trusted host key, and the remote app-server path. Real peer execution remains
unqualified. See `./dist/codex-devices --help` for the required options.
