export type CompanyAuthUser = {
  readonly displayName: string;
  readonly email: string;
};

export type CompanyCatalogItem = {
  readonly tool: string;
  readonly label: string;
  readonly protocol: string;
  readonly defaultModel: string;
  readonly models: readonly string[];
  readonly group: string;
  /** Safe catalog metadata only; never session/API credentials. */
  readonly modelCapabilities?: Readonly<
    Record<string, CompanyModelCapabilities>
  >;
};

export type CompanyReasoningLevel =
  | string
  | {
      readonly effort?: string;
      readonly description?: string;
    };

export type CompanyServiceTier =
  | string
  | {
      readonly id?: string;
      readonly name?: string;
      readonly description?: string;
    };

// Field names match the Rust/catalog schema. DTO normalization accepts camel aliases.
export type CompanyModelCapabilities = {
  readonly supported_reasoning_levels?: readonly CompanyReasoningLevel[];
  readonly default_reasoning_level?: string;
  readonly additional_speed_tiers?: readonly string[];
  readonly service_tiers?: readonly CompanyServiceTier[];
  readonly context_window?: number;
  readonly max_context_window?: number;
  readonly default_service_tier?: string;
};

export type CompanyAuthState =
  | { readonly authenticated: false }
  | {
      readonly authenticated: true;
      readonly user: CompanyAuthUser;
      readonly catalog: readonly CompanyCatalogItem[];
      readonly baseUrl: string | null;
    };

export type CompanyCatalogRefresh = {
  readonly baseUrl: string | null;
  readonly catalog: readonly CompanyCatalogItem[];
};

export type CompanyFeishuLoginStart = {
  readonly url: string;
};

export type CompanyProviderSyncResult = {
  readonly synced: number;
};

export class CompanyAuthError extends Error {
  constructor(
    message: string,
    public readonly kind:
      | "invalid_ticket"
      | "session_expired"
      | "forbidden"
      | "network"
      | "malformed"
      | "unknown",
  ) {
    super(message);
    this.name = "CompanyAuthError";
  }
}
