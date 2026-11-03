# Codex quota migration guide

The Magpie-style `codexQuotaAutoFallback` feature is removed. CC Switch no
longer changes Codex from official login mode to a company or third-party API
when the official account is exhausted. It does not rotate accounts, rewrite
`auth.json`, or automatically bypass upstream capacity and quota limits.

## Replacement: official POST quota protection

Official POST protection is opt-in (disabled by default) with these thresholds:

- 95% five-hour usage
- 98% weekly usage

An explicit `true` remains respected, so existing users who enabled protection stay enabled. The removed legacy fallback setting does not enable protection. When protection is active and a known usage
window reaches either threshold, CC Switch pauses new official POST requests;
third-party model requests are unaffected. The setting applies to requests
passing through CC Switch's local Codex route; it is not an account switch or a
guarantee that an older official model's compaction request will succeed.

Usage is best effort and fails open. If usage cannot be read because of a
network error, expired login, missing window, malformed response, or another
lookup problem, the request is allowed upstream. Usage from official response
headers is reused when available; a cold or stale reading may require one
bounded lookup, but an unavailable lookup never blocks the request.

## Upgrade and recovery notes

Remove operational guidance that expects quota exhaustion to toggle
`requires_openai_auth`, refresh the desktop app-server, or restore a fallback
marker. The old fallback state is not a supported routing mechanism.

If an older transition marker remains after an upgrade, startup treats it as a
recovery hint only when it still matches the current Codex home, route, and
account. Matching state may be restored safely; a stale or mismatched marker is
discarded without overwriting current user settings. This protects against a
crash or a later manual route/account change.

Protection is a local admission guard, not proof of account exhaustion or
successful recovery. A paused official request can resume after usage falls
below the thresholds or a window resets, subject to a fresh reading. Company
and third-party routes still require their own valid credentials, capacity, and
quota.
