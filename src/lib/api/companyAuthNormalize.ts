import {
  CompanyAuthError,
  type CompanyAuthState,
  type CompanyAuthUser,
  type CompanyCatalogItem,
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

const normalizeCatalogItem = (value: unknown): CompanyCatalogItem | null => {
  if (!isRecord(value)) return null;

  const tool = readUiString(value.tool);
  const label = readUiString(value.label);
  const protocol = readUiString(value.protocol);
  const defaultModel = readUiString(value.default_model ?? value.defaultModel);
  const models = readStringArray(value.models);
  const group = readUiString(value.group);

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
