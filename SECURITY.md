# Security

This development preview handles synthetic catalogs. Native sign-in diagnostics
are explicit opt-in operations. Live registry editing and identity repair are
not implemented. Do not give this tool exported credentials or use private
account data as a fixture.

## Report a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/bobbypierce42/codex-devices/security/advisories/new).
If that form is unavailable, open an issue requesting a private contact method
without including vulnerability details. Do not publish exploit details,
credentials, device identifiers, account records, or private screenshots.

Provide affected versions, a synthetic reproducer, expected behavior, actual
behavior, and potential impact. Avoid running a live-account exploit to prove
a report. Acknowledge which observations are inferred or unverified.

## Scope

Reports about incorrect record selection, identity/grant drift, unsafe retries,
credential disclosure, filesystem protection, output handling, or CI permissions
are especially useful. Only the development line is maintained; there is no
published stable release or guaranteed response-time commitment.

Hashes establish integrity against an independently retained value, not provider
authenticity or deleted-record restoration. Local locks do not protect against
every hostile same-user process. See README.md for platform and recovery limits.
