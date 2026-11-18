# Company fork: upstream v4.0.34 integration

## Baseline and scope

The installed application is `4.0.25-company.1`. Its annotated release tag resolves
to commit `877557ecf`, which includes 35 commits absent from GitHub `main`
(`a2c1e5d9e`). This integration starts from that installed release so the catalog,
quota protection and release fixes are retained.

The target is upstream stable tag `v4.0.34` (`d243d970a`), with 305 upstream commits
not in the installed release. At inspection time upstream `main` had another seven
unreleased commits. The company's GitLab branch is a separate history and is not
part of this update.

GitHub scheduled upstream sync has been failing on content conflicts; its attempt
to create a notification issue also failed with `Resource not accessible by
integration`. The latest inspected failure was Actions run `44077639296`.

## Compatibility decisions

- Preserve company login, capability catalog projection, official account handling,
  remote compaction opt-in, safe history migration and company signing identity.
- Keep quota protection enabled by default at 95% for both five-hour and weekly
  windows. Preserve an explicitly disabled setting during upstream's new migration:
  this company release already defaulted on before the upstream migration existed.
- Keep the company's default unified history and retained official authentication,
  while adding upstream history-bucket hints and the optional proxy-without-login mode.
- Adopt upstream's nonblocking startup database maintenance and keep company desktop
  refresh handling.
- Preserve company terminal selection and tests across upstream's command-module split.
- Gate new background downloads and cached-package installation against the version
  selected by the authenticated company update service. A signed GitHub latest
  artifact must not bypass company stable/canary channel selection. Fresh company
  authorization is required even for cached packages; if the company service is
  unavailable, installation waits. Cached fallback only covers a subsequent GitHub
  check failure after successful company approval.

## Relevant upstream changes

- Retry Codex compaction without `tool_choice` when an upstream rejects it.
- Rectify upstream-issued encrypted state rejected after HTTP 200, before output is
  delivered, then retry once on the same provider.
- Map Codex models per target when failing over across vendors; retain image-input
  capability and official display names in generated catalogs.
- Recover poisoned configuration/database locks and move startup maintenance off
  asynchronous workers.
- Add GitLab/Gitea/Forgejo skill repositories and improve S3-compatible storage.

## Delivery boundaries

This is a source integration and validation branch. It does not create a company
release tag, publish update manifests, promote the production update channel or
replace the installed application. Existing long-session routing and compaction
issues require a separate live validation; a successful source merge does not prove
that a provider accepts a particular context size.
