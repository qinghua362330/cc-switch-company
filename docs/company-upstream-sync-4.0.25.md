# Company fork: upstream v4.0.25 integration

This update merges the official stable `v4.0.25` tag into the company fork,
starting from company commit `a2c1e5d9`. It does not track unreleased upstream
`main`, create a company release tag, publish updater manifests, or install the
application on a user's machine.

## Compatibility choices

- Preserve company account/catalog integration, verified remote-compaction V2
  opt-in, login-refresh ownership checks, and company updater/signing identity.
- Remove the Magpie-style `codexQuotaAutoFallback` login-mode switch. Confirmed
  exhaustion no longer changes the Codex login mode automatically.
- Enable upstream official POST quota protection by default in the company build
  (upstream defaults off), with 95% five-hour and 95% weekly cutoffs. An explicit
  `false` remains respected.
- Quota protection pauses new official POST requests at a configured cutoff. It
  is not account failover, does not change `auth.json`, and does not promise that
  an older official model's compaction request will succeed.
- Unknown usage fails open: lookup errors, expired login material, missing
  windows, or otherwise unavailable usage data allow the request upstream.
- Preserve safe startup recovery for a stale quota-transition marker: restore is
  attempted only when the marker still matches the Codex home, route, and account.
  A mismatch is discarded without overwriting current settings.
- Preserve native company terminal launching while adopting the upstream Claude
  terminal launch signature and environment handling.

Regression coverage includes protection defaults and explicit disablement,
unknown-usage admission, stale-marker-safe startup handling, and nonretryable
classification of the official route's quota-protection error.

## Delivery boundaries

The update branch and PR trigger CI, not release publication. Company release
creation remains a separate operation after validation. This source merge is not
proof that switching a long-running Codex chat from official authentication to a
company model resolves pre-turn compaction or capacity failures.

The repository's existing scheduled `upstream-sync.yml` is unchanged: it defaults
to upstream `main` and automatic release mode. That independent automation can
advance beyond this stable snapshot; it was not dispatched by this update.
