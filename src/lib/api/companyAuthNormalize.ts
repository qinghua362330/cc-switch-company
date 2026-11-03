import {
  CompanyAuthError,
  type CompanyAuthState,
  type CompanyAuthUser,
  type CompanyCatalogItem,
  type CompanyModelCapabilities,
  type CompanyCapabilitySource,
  type CompanyCatalogRefresh,
  type CompanyFeishuLoginStart,
} from "@/lib/api/companyAuthTypes";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const readString = (value: unknown): string | null =>
  typeof value === "string" && value.trim().length > 0 ? value : null;

const AUTHORIZATION_PREFIX = ["Authorization:", "Bearer"].join(" ");
const SESSION_TOKEN_PATTERN = ["test_session", "token"].join("_");
const SESSION_TOKEN_KEY_PATTERN = ["session", "token"].join("[_-]?");
const API_KEY_PATTERN = ["api", "key"].join("[_-]?");
const KEY_VALUE_SUFFIX = String.raw`(?:\s*[:=]\s*[^,\s;)}\]]+)?`;
const SECRET_PATTERNS = [
  {
    pattern: new RegExp(`${AUTHORIZATION_PREFIX}(?:\\s+\\S+)?`, "gi"),
    replacement: "<redacted-authorization-header>",
  },
  {
    pattern: /sk-[A-Za-z0-9._-]+/gi,
    replacement: "sk-[redacted]",
  },
  {
    pattern: new RegExp(SESSION_TOKEN_PATTERN, "gi"),
    replacement: "[redacted-session-credential]",
  },
  {
    pattern: new RegExp(
      `${SESSION_TOKEN_KEY_PATTERN}${KEY_VALUE_SUFFIX}`,
      "gi",
    ),
    replacement: "[redacted-session-credential]",
  },
  {
    pattern: new RegExp(`${API_KEY_PATTERN}${KEY_VALUE_SUFFIX}`, "gi"),
    replacement: "[redacted-api-credential]",
  },
] as const;

const redactUiString = (value: string): string =>
  SECRET_PATTERNS.reduce(
    (redacted, { pattern, replacement }) =>
      redacted.replace(pattern, replacement),
    value,
  );

const readUiString = (value: unknown): string | null => {
  const text = readString(value);
  return text ? redactUiString(text) : null;
};

const readStringArray = (value: unknown): readonly string[] =>
  Array.isArray(value)
    ? value
        .filter((item): item is string => typeof item === "string")
        .map(redactUiString)
    : [];

const normalizeCapabilityOptions = (
  value: unknown,
  fields: readonly string[],
) =>
  Array.isArray(value)
    ? value.flatMap<string | Record<string, string>>((item) => {
        if (typeof item === "string") return [redactUiString(item)];
        if (!isRecord(item)) return [];
        const pairs = fields.flatMap((field) => {
          const text = readUiString(item[field]);
          return text === null ? [] : [[field, text]];
        });
        return pairs.length ? [Object.fromEntries(pairs)] : [];
      })
    : undefined;

// These are identifiers, not display text or executable native instructions.
const readMetadataIdentifier = (value: unknown): string | null => {
  if (typeof value !== "string") return null;
  const text = value.trim();
  return /^[A-Za-z0-9][A-Za-z0-9._:/+-]{0,255}$/.test(text) &&
    redactUiString(text) === text
    ? text
    : null;
};

// Fixed-depth allowlist for builder provenance; no arbitrary nested payloads.
const normalizeSourceArtifact = (value: unknown, fields: readonly string[]) => {
  if (!isRecord(value)) return undefined;
  const normalized: Record<string, string> = {};
  for (const field of fields) {
    const text = readMetadataIdentifier(value[field]);
    if (
      text !== null &&
      (field !== "sha256" || /^[a-fA-F0-9]{64}$/.test(text))
    ) {
      normalized[field] = field === "sha256" ? text.toLowerCase() : text;
    }
  }
  return Object.keys(normalized).length ? normalized : undefined;
};

const normalizeCapabilitySource = (value: unknown) => {
  if (!isRecord(value)) return undefined;
  const normalized: {
    -readonly [K in keyof CompanyCapabilitySource]: CompanyCapabilitySource[K];
  } = {};
  const schemaVersion = value.schema_version ?? value.schemaVersion;
  if (
    typeof schemaVersion === "number" &&
    Number.isSafeInteger(schemaVersion) &&
    schemaVersion > 0
  ) {
    normalized.schema_version = schemaVersion;
  }
  for (const [snake, camel] of [
    ["version", "version"],
    ["codex_version", "codexVersion"],
    ["model_slug", "modelSlug"],
    ["digest", "digest"],
    ["fingerprint", "fingerprint"],
  ] as const) {
    const text = readMetadataIdentifier(value[snake] ?? value[camel]);
    if (text !== null) normalized[snake] = text;
  }
  if (isRecord(value.fields)) {
    const fields: Record<string, string> = {};
    for (const field of [
      "context_window",
      "max_context_window",
      "input_modalities",
      "max_input_tokens",
      "max_output_tokens",
      "supported_reasoning_levels",
      "default_reasoning_level",
      "additional_speed_tiers",
      "service_tiers",
      "default_service_tier",
      "catalog_snapshot",
    ]) {
      const source = readMetadataIdentifier(value.fields[field]);
      if (source !== null) fields[field] = source;
    }
    if (Object.keys(fields).length) normalized.fields = fields;
  }
  const bundled = normalizeSourceArtifact(value.codex_bundled, [
    "kind",
    "sha256",
    "codex_version",
  ]);
  if (bundled) normalized.codex_bundled = bundled;
  const modelsDev = normalizeSourceArtifact(value.models_dev, [
    "kind",
    "sha256",
    "provider",
    "model_id",
  ]);
  if (modelsDev) normalized.models_dev = modelsDev;
  return Object.keys(normalized).length ? normalized : undefined;
};

// A deterministic, non-security change detector until every server supplies a
// digest. Sort object keys so transport ordering cannot trigger a false refresh.
// Only the fingerprint crosses into React, never native prompt/tool contents.
const snapshotFingerprint = (value: Record<string, unknown>): string => {
  const canonicalize = (item: unknown): unknown => {
    if (Array.isArray(item)) return item.map(canonicalize);
    if (!isRecord(item)) return item;
    return Object.fromEntries(
      Object.keys(item)
        .sort()
        .map((key) => [key, canonicalize(item[key])]),
    );
  };
  const serialized = JSON.stringify(canonicalize(value));
  let hash = 0xcbf29ce484222325n;
  for (let i = 0; i < serialized.length; i++) {
    hash = BigInt.asUintN(
      64,
      (hash ^ BigInt(serialized.charCodeAt(i))) * 0x100000001b3n,
    );
  }
  return `fnv1a64:${hash.toString(16).padStart(16, "0")}`;
};

const normalizeCatalogSnapshot = (value: unknown) => {
  if (!isRecord(value)) return undefined;
  const schemaVersion = value.schema_version ?? value.schemaVersion;
  const codexVersion = readMetadataIdentifier(
    value.codex_version ?? value.codexVersion,
  );
  const modelSlug = readMetadataIdentifier(
    isRecord(value.model)
      ? value.model.slug
      : (value.model_slug ?? value.modelSlug),
  );
  if (schemaVersion !== 1 || !codexVersion || !modelSlug) return undefined;
  const digest = readMetadataIdentifier(value.digest);
  const source = normalizeSourceArtifact(value.source, ["kind", "sha256"]);
  return {
    schema_version: 1 as const,
    codex_version: codexVersion,
    model_slug: modelSlug,
    ...(digest ? { digest } : {}),
    ...(source ? { source } : {}),
    // Canonicalize aliases as well as object ordering before fingerprinting.
    fingerprint: snapshotFingerprint({
      schema_version: 1,
      codex_version: codexVersion,
      model: isRecord(value.model) ? value.model : { slug: modelSlug },
    }),
  };
};

const normalizeModelCapabilities = (
  value: unknown,
): Readonly<Record<string, CompanyModelCapabilities>> | undefined => {
  if (!isRecord(value)) return undefined;
  const capabilities: Record<string, CompanyModelCapabilities> = {};
  // Stable ordering makes snake/camel and reordered maps compare equally.
  for (const model of Object.keys(value).sort()) {
    const raw = value[model];
    if (!isRecord(raw)) continue;
    const normalized: Record<string, unknown> = {};
    for (const [snake, camel] of [
      ["supported_reasoning_levels", "supportedReasoningLevels"],
      ["default_reasoning_level", "defaultReasoningLevel"],
      ["additional_speed_tiers", "additionalSpeedTiers"],
      ["service_tiers", "serviceTiers"],
      ["context_window", "contextWindow"],
      ["max_context_window", "maxContextWindow"],
      ["max_input_tokens", "maxInputTokens"],
      ["max_output_tokens", "maxOutputTokens"],
      ["input_modalities", "inputModalities"],
      ["default_service_tier", "defaultServiceTier"],
    ] as const) {
      const field = raw[snake] ?? raw[camel];
      if (field === undefined || field === null) continue;
      if (snake === "supported_reasoning_levels" || snake === "service_tiers") {
        const options = normalizeCapabilityOptions(
          field,
          snake === "supported_reasoning_levels"
            ? ["effort", "description"]
            : ["id", "name", "description"],
        );
        if (options !== undefined) normalized[snake] = options;
      } else if (snake === "additional_speed_tiers") {
        if (Array.isArray(field)) normalized[snake] = readStringArray(field);
      } else if (snake === "input_modalities") {
        if (Array.isArray(field)) {
          normalized[snake] = field.flatMap((item) => {
            const modality = readMetadataIdentifier(item);
            return modality === null ? [] : [modality];
          });
        }
      } else if (
        snake === "context_window" ||
        snake === "max_context_window" ||
        snake === "max_input_tokens" ||
        snake === "max_output_tokens"
      ) {
        if (
          typeof field === "number" &&
          Number.isSafeInteger(field) &&
          field > 0
        )
          normalized[snake] = field;
      } else {
        const text = readUiString(field);
        if (text !== null) normalized[snake] = text;
      }
    }
    const snapshot = normalizeCatalogSnapshot(
      raw.catalog_snapshot ?? raw.catalogSnapshot,
    );
    if (snapshot) normalized.catalog_snapshot = snapshot;
    for (const field of ["source", "provenance"] as const) {
      const source = normalizeCapabilitySource(raw[field]);
      if (source) normalized[field] = source;
    }
    // Only the declared capability fields cross into React; unknown secret fields do not.
    capabilities[redactUiString(model)] = normalized;
  }
  return capabilities;
};

const normalizeCatalogItem = (value: unknown): CompanyCatalogItem | null => {
  if (!isRecord(value)) return null;

  const tool = readUiString(value.tool);
  const label = readUiString(value.label);
  const protocol = readUiString(value.protocol);
  const defaultModel = readUiString(value.default_model ?? value.defaultModel);
  const models = readStringArray(value.models);
  const group = readUiString(value.group);
  const modelCapabilities = normalizeModelCapabilities(
    value.model_capabilities ?? value.modelCapabilities,
  );

  if (
    !tool ||
    !label ||
    !protocol ||
    !defaultModel ||
    models.length === 0 ||
    !group
  ) {
    return null;
  }

  return {
    tool,
    label,
    protocol,
    defaultModel,
    models,
    group,
    ...(modelCapabilities !== undefined ? { modelCapabilities } : {}),
  };
};

const normalizeCatalog = (
  value: unknown,
): readonly CompanyCatalogItem[] | null => {
  if (!Array.isArray(value)) return null;

  const catalog: CompanyCatalogItem[] = [];
  for (const item of value) {
    const normalized = normalizeCatalogItem(item);
    if (!normalized) return null;
    catalog.push(normalized);
  }
  return catalog;
};

const normalizeUser = (value: unknown): CompanyAuthUser | null => {
  if (!isRecord(value)) return null;

  const displayName = readUiString(value.display_name ?? value.displayName);
  const email = readUiString(value.email);
  if (!displayName || !email) return null;

  return { displayName, email };
};

export function normalizeCompanyAuthState(value: unknown): CompanyAuthState {
  if (value === null || value === undefined) {
    return { authenticated: false };
  }
  if (!isRecord(value)) {
    throw new CompanyAuthError(
      "认证服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  const authenticated = value.authenticated !== false && Boolean(value.user);
  if (!authenticated) {
    return { authenticated: false };
  }

  const user = normalizeUser(value.user);
  const catalog = normalizeCatalog(value.catalog);
  if (!user || !catalog) {
    throw new CompanyAuthError(
      "认证服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  return {
    authenticated: true,
    user,
    catalog,
    baseUrl: readString(value.base_url ?? value.baseUrl),
  };
}

export function normalizeCompanyCatalogRefresh(
  value: unknown,
): CompanyCatalogRefresh {
  if (!isRecord(value)) {
    throw new CompanyAuthError(
      "目录服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  const catalog = normalizeCatalog(value.catalog);
  if (!catalog) {
    throw new CompanyAuthError(
      "目录服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  return {
    catalog,
    baseUrl: readString(value.base_url ?? value.baseUrl),
  };
}

export function normalizeCompanyFeishuLoginStart(
  value: unknown,
): CompanyFeishuLoginStart {
  if (!isRecord(value)) {
    throw new CompanyAuthError(
      "认证服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  const url = readString(value.url);
  if (!url) {
    throw new CompanyAuthError(
      "认证服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  return { url };
}
