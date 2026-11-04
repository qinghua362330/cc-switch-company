# Codex official quota and conversation migration (company build)

The Magpie-style `codexQuotaAutoFallback` login-mode switch has been removed.
The company build enables upstream official POST quota protection by default:
95% five-hour usage or 95% weekly usage. An explicit `false` wins. All official
POST requests routed through CC Switch, **including pre-turn compaction**, are
subject to the guard. Unknown usage fails open; concurrent use, other clients
and expired credentials can still exhaust the account. It is not account
failover and never reroutes a previous official model's compaction request.

This policy does not make an existing official-auth conversation portable. On an official-to-company model switch, Codex may first compact with the **previous official model**. That compaction produces opaque OpenAI state which a third-party backend cannot decrypt. CC Switch must not reinterpret it as a usable summary or claim the old context was transferred. For existing conversations, start a new company-model conversation, or ask the official model to write a plaintext handoff (e.g. `NOTES.md`) before switching and have the new model read it.

A future gateway-managed portable-summary mode requires explicit, verifiable eligibility established from the conversation's first turn, plus request/response pairing and safe retry behavior. The request preflight helper alone is not an active migration feature: no official route enables it today. Unknown or foreign encrypted compaction entries remain ineligible.
