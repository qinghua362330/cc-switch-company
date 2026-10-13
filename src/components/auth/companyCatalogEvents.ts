export const COMPANY_CATALOG_SELECT_EVENT = "company-auth-catalog-select";
// Login completed in Rust. This event intentionally contains no session data.
export const COMPANY_AUTH_LOGIN_EVENT = "company-auth-login";

export type CompanyCatalogSelectDetail = {
  readonly tool: string;
  readonly label: string;
};
