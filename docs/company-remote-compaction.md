# Company Remote Compaction V2

## Scope

The verified production route is HTTPS `leharrt.com/v1`, model
`gpt-5.6-sol`, using streaming Responses. The company catalog currently mixes
this model with unverified GPT aliases, DeepSeek, Gemini, and GLM models.
Compaction is a provider-level capability, not a model-level catalog property.

The existing mixed card remains unchanged and selected. Catalog refresh adds a
separate company card whose label ends in `远程压缩 V2`, containing only the
verified model. Select that card explicitly. Refresh preserves its identity and
rotates its credentials, including when the parent catalog later becomes
single-model. A single-model verified company card also receives V2 directly.

Generated configuration keeps the company display name and uses the selected
provider ID:

```toml
model_provider = "custom"
model = "gpt-5.6-sol"

[model_providers.custom.capabilities]
remote_compaction = "v2"
```

No `disable_response_storage` setting or enabled `supports_websockets` setting
is added. Existing company storage settings are removed on refresh, enabled
WebSocket settings are removed, and explicit disabled WebSocket settings survive.
The obsolete `features.remote_compaction_v2` key is removed without removing
other features. Non-company cards are not normalized by this feature.

V2 is not inferred from a model prefix or an OpenAI-compatible URL. A changed
route or unverified external model catalog revokes automatic V2. In Stack mode,
when an explicitly enabled company V2 card participates, the effective provider
uses the capability intersection, so that card cannot grant V2 to unverified
members. Existing Stack configurations without that opt-in retain their prior
behavior; this does not fix all pre-existing official-provider defaults. The
original cards are not rewritten by the projection. Built-in official routing
without a writable provider capability table cannot safely express this new
mixed configuration and is rejected without changing the session selector.

## Evidence And Limits

On 2026-11-02 the separate production probe returned encrypted `compaction`
items and completed successfully for text, user-image, and tool-image fixtures.
Each continuation recovered a random code present only in discarded assistant
history. NewAPI request pairs:

- Text: `202611020840279480996288268d9d6M7WL6UM1`,
  `202611020840328956377478268d9d6cvgaCvrc`.
- User image: `202611020840349078117488268d9d6Uqt80YqQ`,
  `202611020840404914502518268d9d6Fdyx7FWW`.
- Tool image: `202611020841190045858818268d9d63lhUGqvU`,
  `202611020841280045759788268d9d63UGYNMxh`.

These are controlled functional samples, not a 12-round performance benchmark.
User-attached image content remained after compaction; tool-output image history
was discarded by the fixture. No guarantee is made that every image is removed.

The desktop engine `0.166.0-alpha.5.1` supports the explicit capability and sends
`compaction_trigger` to `/responses`. Local engines `0.142.5` and independently
installed `1.3.0` accepted but ignored it, using ordinary summary requests.
Renaming the provider to `OpenAI` is deliberately avoided: the local `1.3.0`
engine then used legacy `/responses/compact`, which the tested production route
does not support. These version results are local observations, not a general
compatibility guarantee for untested engines.

The protocol acceptance test uses an isolated HOME, fake key and loopback server
with the actual Rust-generated configuration. It must observe trigger,
compaction output, completed response and continuation without old assistant
history, not merely a successful HTTP status. Production gateway services and
the user's current Codex session are not changed by this test.
