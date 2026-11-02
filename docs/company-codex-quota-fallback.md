# Company Codex Quota Fallback

This default-on setting follows Magpie's quota-triggered API switching approach,
adapted to CC Switch's provider-scoped bearer credentials. It does not rotate
official accounts, change transport, or bypass upstream quotas.

## Behavior

- **Codex settings > Automatic login mode switching** defaults to on, including
  upgrades where the setting is absent. Requires an existing official login and
  independent company/third-party API credentials. Direct connections additionally
  require the non-takeover official-login preservation setting. Aggregation with
  verified local proxy takeover always preserves the official login and does not
  depend on that setting.
- New and refreshed company configs default to `requires_openai_auth = true`.
- While CC Switch is running, check the logged-in official account every five
  minutes. Only confirmed quota exhaustion with no usable credits activates
  fallback. Network/authentication errors and ambiguous responses do nothing.
- On a company/third-party route, including aggregation through local proxy
  takeover, temporarily change `requires_openai_auth` from `true` to `false`.
  Keep the existing API credentials, endpoint, model, and `auth.json` untouched.
  Aggregation must have independent company/third-party API authentication;
  official-only credentials cannot bypass the official account's quota.
- Restore the flag after confirmed recovery, or when the feature is disabled,
  only if the account, provider and model/route configuration still match the
  automatically changed version. A stable JWT user subject plus workspace ID
  retains ownership across token refresh; opaque tokens are tracked conservatively.
- Persist hashes (not tokens or configuration contents) in
  `codex-quota-fallback.json` in the application config directory. Exact
  route refreshes retain the override; backfill strips it from the
  stored provider preference.
- Explicit API mode on non-company cards remains respected. Company config
  normalization writes `true`; while an owned automatic switch is active, a
  same-route refresh keeps its temporary `false` until quota recovery.

## Background Refresh

Changing the config does not by itself update the desktop's cached login mode.
After a switch or restoration, CC Switch requests a background-service refresh:

- `codex-quota-fallback-changed` reports the config change or removal of the
  automatic override. Automatic refresh is next; it is not an immediate request
  to restart the app or evidence that sending works.
- `codex-login-mode-refresh` with `status: "waiting"` means waiting for the
  current turn to finish before refreshing the background service.
- `status: "reloaded"` means the old background service has exited (or none
  remains) and the config is ready. It does **not** prove a replacement service
  has loaded the config or the send button is unlocked.
- `status: "manual"` means automatic refresh cannot be completed safely.
  The notification asks the user to restart Codex manually.

Refresh intent is persisted in `codex-login-mode-refresh.json` before the config
write, for both fallback activation and restoration. It records the managed home
and route/account fingerprints, not credentials or full config contents. On
CC Switch restart, matching intent resumes even if restoration already removed
the fallback marker. A different home, route, or account prevents replay; a crash
before the config write leaves mismatched intent that is not resumed.

Automatic refresh currently supports **macOS only**, subject to verified process
identity. Other platforms report `manual`; they do not attempt to end a process.
Automatic refresh targets only the verified desktop background `app-server`
for the managed Codex home, never the whole application. It waits until no
request or open turn is active and the idle guard is satisfied. Support depends
on the platform's ability to verify process identity and safely end that exact
service; unsupported platforms and unverifiable processes require manual
restart. This is not a blanket cross-platform automatic-refresh guarantee.

An unknown session/turn state is never treated as idle. Three consecutive
session-inspection failures for the pending refresh produce `manual`; a
successful inspection resets that counter. A known open turn continues waiting.
Process-discovery failures report `manual` immediately, and captured services
still present after three termination attempts also require manual restart.

These are implementation and test contracts, not verification in the actual
Codex UI. Neither refresh intent nor `reloaded` establishes that the send button
is enabled or that a real turn succeeds.

## Boundaries

- Company/third-party routes support both direct mode and local proxy aggregation.
  Official routes, profile overrides, and routes without independent API
  authentication are not changed.
- Requires CC Switch to remain running. There is no token refresh or automatic
  login; expired official tokens produce unknown quota, not a false recovery.
- Quota checks use file-backed `auth.json` in the same managed Codex home.
  Explicit `keyring`, `auto`, or `ephemeral` credential stores are skipped
  rather than risking a different account's global keychain credentials.
- A config-change or background-service notification is not proof of a live
  send. If automatic refresh is unsafe or unsupported, restart Codex manually.
- Manual routing/account/provider edits invalidate automatic restoration.
  Unrelated preferences and MCP edits are retained when patching the flag;
  no stale full-config snapshot is written over user changes.
- This addresses the desktop's official-account quota gate when using a paid
  company/third-party API. Model and company APIs still require their own
  available quota and valid credentials. It does not fix relay capacity,
  image-history growth, latency, or upstream rate limits.

## Source Evidence

- [Magpie issue #540](https://github.com/yetone/magpie/issues/540) describes the
  desktop blocking third-party sends when the signed-in ChatGPT account is
  exhausted. The [original quota-switch commit](https://github.com/yetone/magpie/commit/27f0e4ebdd26d78284dfb16d7c7b3e029f7a91d7)
  introduced periodic switching and restoration.
- [Commit 8b0132b](https://github.com/yetone/magpie/commit/8b0132b3d2cf20db65a7a4a83b038dc8955c53bd)
  explains the cached `authMethod`: the desktop's composer checks ChatGPT
  identity and quota, while `account/read` is refreshed when its app-server
  starts. It replaces whole-app reopening with a clean background app-server
  exit after idle/turn checks, in both switching directions. Its reported real-app
  checks did not include an actually exhausted account, so they are not proof
  of an unlocked send button here.
- [Commit 72ca3d5](https://github.com/yetone/magpie/commit/72ca3d593bd527b608153886ee8d15e282ac7530)
  explains preserving `requires_openai_auth = true` while quota remains,
  keeping ChatGPT chats/projects available, and disabling it during exhaustion.
  Its sandbox checks kept `auth.json` unchanged; this is upstream evidence,
  not a live verification of CC Switch.

Date clarification: these two commits are dated October 27 and 28, 2026 in
GitHub's commit metadata, respectively, not October 23.
