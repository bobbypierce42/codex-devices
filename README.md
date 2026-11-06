# Codex Devices

Codex Devices is a macOS app for reviewing and renaming computer labels in your
Codex account. It can help when two Macs have the same label or a computer keeps
an old label after a migration.

**Development preview:** The app uses an unsupported internal API. Its account
and rename workflows have synthetic tests, but live acceptance remains incomplete.
This independent project is not affiliated with OpenAI.

## What it does

- Shows the account's computer registrations, including offline entries.
- Prepares a private backup and a review before changing one selected label.
- Optionally finds Macs and compares their system names with account registrations.

The label is the name shown in Codex. Changing it does not rename the Mac or
remove an old registration. Live cleanup is not implemented. Cloud, unsupported,
and ambiguous registrations remain read only.

## Requirements

- macOS 13 or later, with the [Xcode command-line tools](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools/)
  and Swift 5.9 or later.
- Rust 1.89 or later and Cargo. Install them with [rustup](https://rustup.rs/).
- For a real account, Codex.app or ChatGPT.app in `/Applications`, signed in to
  the intended ChatGPT account. If both are installed, the app prefers Codex.app.

There is no prebuilt release. Build the app from this repository on the Mac
where you want to use it.

## Install

1. On the repository's GitHub page, choose **Code**, then **Download ZIP**.
   Extract the download. You can also clone the repository with Git.
2. Open Terminal in the extracted repository folder. One way is to type `cd`
   followed by a space, drag the folder into the Terminal window, and press Return.
3. Build the app:

   ```sh
   bash ./script/build_and_run.sh --build-only
   ```

4. Open `dist/Codex Devices.app` in Finder. The app loads the signed-in account's
   computer list when it opens.

To rebuild, first quit any running copy of the app in this repository's `dist`
folder. The build script refuses to replace that running app.

## Try it with invented data

After building, run this command from the repository folder:

```sh
open -n 'dist/Codex Devices.app' --args --demo
```

The window identifies itself as a synthetic demo and shows an invented computer.
It uses simulated account and discovery services; it does not access a live
account or change real registrations. Quit the demo before opening the app for
an actual account.

## Rename a Codex label

1. In **Codex computers**, select the personal computer you want to rename. The
   list includes online and offline registrations. Check the full registration
   and installation fingerprint: two entries can have the same label. Use
   **Find Macs**, described below, if you need to match a Mac first.
2. Enter the new **Proposed Codex label**, then choose **Prepare Backup & Review**.
   Preparing does not send a rename request.
3. Review the current and proposed labels, registration, fingerprint, and
   backup location. Select the review checkbox, then choose
   **Apply Reviewed Change**.
4. Wait for **Verified**. This requires a matching response and fresh account
   readback that preserves IDs, permissions, attachments, and other records.
   If the result is uncertain, use **Check Readback** to check again without
   repeating the rename request. Keep the operation folder for investigation
   if the outcome remains uncertain.

The app saves private backups and operation journals in
`~/Library/Application Support/Codex Devices/AccountOperations`. These files
contain account metadata. Keep them private. A metadata backup does not promise
an automatic rollback.

### Compare a Mac's system name

To compare with a Mac's system name, use the optional **Find Macs** tab and choose
**Scan Network**. Discovery uses Bonjour, an existing Tailscale installation and
an optional hostname. **Inspect** confirms the exact target and account first.
Remote inspection uses existing SSH access and explicit host-key trust; changed
keys refuse. Its secure prompt distinguishes that remote account's password
from a local SSH key passphrase. Local inspection uses the current Mac account.
An unambiguous match can copy the inspected system name into the account editor;
the same backup and name review still follow. No network inspection starts
automatically, and custom labels are not rewritten automatically.

## Command-line use

The project also includes a macOS command-line tool for account-label changes
and a Rust tool for offline simulations. See the [CLI guide](docs/cli.md) for
setup, examples, and the differences between them.

## Contributing and support

See [CONTRIBUTING.md](CONTRIBUTING.md) for development checks and
[SECURITY.md](SECURITY.md) for reporting security concerns. Linux and Windows
are not qualified. The project is available under the [MIT license](LICENSE).
