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
- Treat upstream's `nothing_to_migrate` history result as a successful no-op, so
  switching providers does not incorrectly report a postponed migration. Only use
  the fast empty-official-bucket check after the first company migration: legacy
  company/official buckets must still migrate when no `openai` rows exist.
- Make tests explicitly choose the upstream non-unified-history scenario where
  required; production company defaults remain unchanged.
- Use the shared application list in test mocks, supply new auto-update endpoints,
  and bring all eight locales into parity. Remove only the unused
  `company_auth_catalog_version` IPC wrapper/registration; authenticated catalog
  refresh and catalog metadata in authentication state remain available.
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

## Validation (2026-11-18)

- Frontend: all 324 test files / 4,302 tests have passing results. The initial full
  run hit its outer time limit after 226 files; the remaining 98 files were run
  separately. Failures were repaired and rechecked. Two Pi form timeouts passed
  in an isolated 55-test run with one worker and a 45-second per-test limit.
- Backend: the initial full nextest run executed 5,256 tests: 5,249 passed,
  seven failed, and 16 were skipped. After repairs, all 222 tests in history
  migration, routing controller, provider service and update authorization passed.
  The previously passing unrelated tests were not repeated.
- Rust Clippy: all targets with `test-hooks`, warnings denied, passed on the final
  source. Rustfmt and `git diff --check` passed.
- TypeScript, renderer production build and source Prettier checks passed.
- Company release/update/history/compaction script regressions: 61 passed.

The global pre-push hook also starts a signed Tauri release build. This machine
has no updater signing private key, so that packaging run was stopped; it is not
reported as a successful desktop package build. Source delivery uses the hook's
one-invocation `ECC_SKIP_PREPUSH=1` option after the manual checks above. The hook
configuration is unchanged. Signed installer packaging and multi-platform CI
remain release validation tasks.
