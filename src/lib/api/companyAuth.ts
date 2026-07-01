import { invoke } from "@tauri-apps/api/core";
import {
  normalizeCompanyAuthState,
  normalizeCompanyCatalogRefresh,
  normalizeCompanyFeishuLoginStart,
} from "@/lib/api/companyAuthNormalize";
import { CompanyAuthError } from "@/lib/api/companyAuthTypes";
import type {
  CompanyAuthState,
  CompanyCatalogRefresh,
  CompanyFeishuLoginStart,
  CompanyProviderSyncResult,
} from "@/lib/api/companyAuthTypes";

export { CompanyAuthError } from "@/lib/api/companyAuthTypes";
export type {
  CompanyAuthState,
  CompanyAuthUser,
  CompanyCatalogItem,
  CompanyCatalogRefresh,
  CompanyFeishuLoginStart,
  CompanyProviderSyncResult,
} from "@/lib/api/companyAuthTypes";
export {
  normalizeCompanyAuthState,
  normalizeCompanyCatalogRefresh,
  normalizeCompanyFeishuLoginStart,
} from "@/lib/api/companyAuthNormalize";

export function toCompanyAuthError(error: unknown): CompanyAuthError {
  if (error instanceof CompanyAuthError) return error;

  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : "";
  const lower = raw.toLowerCase();

  if (
    lower.includes("401") ||
    lower.includes("invalid") ||
    lower.includes("unauthorized")
  ) {
    return new CompanyAuthError(
      "Ticket 无效或已过期，请重新获取。",
      "invalid_ticket",
    );
  }
  if (lower.includes("403") || lower.includes("forbidden")) {
    return new CompanyAuthError(
      "当前飞书租户无权限使用公司客户端。",
      "forbidden",
    );
  }
  if (
    lower.includes("failed to fetch") ||
    lower.includes("network") ||
    lower.includes("offline")
  ) {
    return new CompanyAuthError(
      "暂时无法连接认证服务，请检查网络后重试。",
      "network",
    );
  }
  if (lower.includes("malformed") || lower.includes("格式异常")) {
    return new CompanyAuthError(
      "认证服务返回格式异常，请稍后重试。",
      "malformed",
    );
  }

  return new CompanyAuthError("登录失败，请稍后重试。", "unknown");
}

export async function getCompanyAuthState(): Promise<CompanyAuthState> {
  try {
    return normalizeCompanyAuthState(await invoke("company_auth_get_state"));
  } catch (error) {
    throw toCompanyAuthError(error);
  }
}

export async function loginWithCompanyTicket(
  ticket: string,
): Promise<CompanyAuthState> {
  const normalizedTicket = ticket.trim();
  if (!/^fs_[A-Za-z0-9._-]+$/.test(normalizedTicket)) {
    throw new CompanyAuthError(
      "请输入 fs_ 开头的一次性 ticket。",
      "invalid_ticket",
    );
  }

  try {
    const state = normalizeCompanyAuthState(
      await invoke("company_auth_login_with_ticket", {
        ticket: normalizedTicket,
      }),
    );
    if (!state.authenticated) {
      throw new CompanyAuthError(
        "认证服务返回格式异常，请稍后重试。",
        "malformed",
      );
    }
    return state;
  } catch (error) {
    throw toCompanyAuthError(error);
  }
}

export async function refreshCompanyCatalog(): Promise<CompanyCatalogRefresh> {
  try {
    return normalizeCompanyCatalogRefresh(
      await invoke("company_auth_refresh_catalog"),
    );
  } catch (error) {
    throw toCompanyAuthError(error);
  }
}

export async function startCompanyFeishuLogin(): Promise<CompanyFeishuLoginStart> {
  try {
    return normalizeCompanyFeishuLoginStart(
      await invoke("company_auth_start_feishu_login"),
    );
  } catch (error) {
    throw toCompanyAuthError(error);
  }
}

export async function syncCompanyProviders(): Promise<CompanyProviderSyncResult> {
  try {
    return await invoke<CompanyProviderSyncResult>(
      "company_auth_sync_providers",
    );
  } catch (error) {
    throw toCompanyAuthError(error);
  }
}

export async function logoutCompanyAuth(): Promise<void> {
  await invoke("company_auth_logout");
}
