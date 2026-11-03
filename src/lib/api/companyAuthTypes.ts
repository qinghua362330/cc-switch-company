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

export type CompanyCapabilitySourceArtifact = {
  readonly kind?: string;
  readonly sha256?: string;
  readonly codex_version?: string;
  readonly provider?: string;
  readonly model_id?: string;
};

// UI-safe descriptors only; the full native model remains in backend/DB config.
export type CompanyCapabilitySource = {
  readonly fields?: Readonly<Record<string, string>>;
  readonly codex_bundled?: CompanyCapabilitySourceArtifact;
  readonly models_dev?: CompanyCapabilitySourceArtifact;
  readonly schema_version?: number;
  readonly version?: string;
  readonly codex_version?: string;
  readonly model_slug?: string;
  readonly digest?: string;
  readonly fingerprint?: string;
};

export type CompanyCatalogSnapshotDescriptor = {
  readonly source?: CompanyCapabilitySourceArtifact;
  readonly schema_version: 1;
  readonly codex_version: string;
  readonly model_slug: string;
  readonly digest?: string;
  readonly fingerprint: string;
};

// Field names match the Rust/catalog schema. DTO normalization accepts camel aliases.
export type CompanyModelCapabilities = {
  readonly catalog_snapshot?: CompanyCatalogSnapshotDescriptor;
  readonly source?: CompanyCapabilitySource;
  readonly provenance?: CompanyCapabilitySource;
  readonly supported_reasoning_levels?: readonly CompanyReasoningLevel[];
  readonly default_reasoning_level?: string;
  readonly additional_speed_tiers?: readonly string[];
  readonly service_tiers?: readonly CompanyServiceTier[];
  readonly context_window?: number;
  readonly max_context_window?: number;
  readonly input_modalities?: readonly string[];
  readonly max_input_tokens?: number;
  readonly max_output_tokens?: number;
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
