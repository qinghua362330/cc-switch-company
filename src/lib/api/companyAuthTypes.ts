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
      | "forbidden"
      | "network"
      | "malformed"
      | "unknown",
  ) {
    super(message);
    this.name = "CompanyAuthError";
  }
}
