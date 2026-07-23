import { http, HttpResponse } from "msw";
import type { Provider } from "@/types";
import { getCompanyAuthState, setCompanyAuthState } from "./companyAuthState";
import { addProvider, getProviders, updateProvider } from "./state";

const TAURI_ENDPOINT = "http://tauri.local";

const parseJsonBody = async (request: Request): Promise<unknown> => {
  try {
    const body = await request.text();
    if (!body) return undefined;
    const parsed: unknown = JSON.parse(body);
    return parsed;
  } catch {
    return undefined;
  }
};

const readTicket = async (request: Request): Promise<string | undefined> => {
  const body = await parseJsonBody(request);
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return undefined;
  }

  const ticket = Object.getOwnPropertyDescriptor(body, "ticket")?.value;
  return typeof ticket === "string" ? ticket : undefined;
};

const success = (payload: unknown): Response =>
  new Response(JSON.stringify(payload) ?? "null", {
    headers: { "Content-Type": "application/json" },
  });

const slugify = (value: string) =>
  value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "") || "pool";

const stableLabelHash = (value: string) => {
  let hash = 0xcbf29ce4;
  for (const byte of new TextEncoder().encode(value)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
};

const syncMockCompanyProviders = () => {
  const state = getCompanyAuthState();
  if (!state || typeof state !== "object" || Array.isArray(state)) {
    return 0;
  }
  const catalog = Object.getOwnPropertyDescriptor(state, "catalog")?.value;
  const baseUrl =
    Object.getOwnPropertyDescriptor(state, "base_url")?.value ??
    Object.getOwnPropertyDescriptor(state, "baseUrl")?.value ??
    "https://leharrt.com";
  if (!Array.isArray(catalog)) return 0;

  const completeCatalog = [...catalog];
  if (
    !completeCatalog.some((item) => {
      if (!item || typeof item !== "object" || Array.isArray(item)) {
        return false;
      }
      const tool = Object.getOwnPropertyDescriptor(item, "tool")?.value;
      return (
        tool === "gemini" || tool === "gemini-cli" || tool === "gemini_cli"
      );
    })
  ) {
    const geminiModels: string[] = [];
    for (const item of completeCatalog) {
      if (!item || typeof item !== "object" || Array.isArray(item)) continue;
      const models = Object.getOwnPropertyDescriptor(item, "models")?.value;
      if (!Array.isArray(models)) continue;
      for (const model of models) {
        if (
          typeof model === "string" &&
          model.toLowerCase().includes("gemini") &&
          !geminiModels.includes(model)
        ) {
          geminiModels.push(model);
        }
      }
    }
    if (geminiModels.length > 0) {
      completeCatalog.push({
        tool: "gemini",
        label: "公司号池 Gemini",
        protocol: "gemini",
        default_model: geminiModels[0],
        models: geminiModels,
        group: "default",
      });
    }
  }

  let synced = 0;
  for (const item of completeCatalog) {
    if (!item || typeof item !== "object" || Array.isArray(item)) continue;
    const tool = Object.getOwnPropertyDescriptor(item, "tool")?.value;
    const label = Object.getOwnPropertyDescriptor(item, "label")?.value;
    const defaultModel =
      Object.getOwnPropertyDescriptor(item, "default_model")?.value ??
      Object.getOwnPropertyDescriptor(item, "defaultModel")?.value;
    if (typeof label !== "string") continue;
    const app =
      tool === "codex"
        ? "codex"
        : tool === "claude"
          ? "claude"
          : tool === "gemini" || tool === "gemini-cli" || tool === "gemini_cli"
            ? "gemini"
            : null;
    if (!app) continue;

    const id = `company-${app}-${slugify(label)}-${stableLabelHash(label)}`;
    const provider: Provider = {
      id,
      name: label,
      settingsConfig:
        app === "codex"
          ? {
              auth: {
                OPENAI_API_KEY: "sk-test-secret-should-not-render",
              },
              config: `model_provider = "custom"\nmodel = "${defaultModel}"\n\n[model_providers.custom]\nname = "${label}"\nbase_url = "${baseUrl.replace(/\/$/, "")}/v1"\nwire_api = "responses"\nrequires_openai_auth = true\n`,
            }
          : app === "gemini"
            ? {
                env: {
                  GOOGLE_GEMINI_BASE_URL: baseUrl,
                  GEMINI_API_KEY: "sk-test-secret-should-not-render",
                  GEMINI_MODEL: defaultModel,
                },
              }
            : {
                env: {
                  ANTHROPIC_BASE_URL: baseUrl,
                  ANTHROPIC_AUTH_TOKEN: "sk-test-secret-should-not-render",
                  ANTHROPIC_MODEL: defaultModel,
                  ANTHROPIC_DEFAULT_HAIKU_MODEL: defaultModel,
                  ANTHROPIC_DEFAULT_SONNET_MODEL: defaultModel,
                  ANTHROPIC_DEFAULT_OPUS_MODEL: defaultModel,
                },
              },
      category: "custom" as const,
      meta: {
        providerType: "company_auth",
        apiFormat:
          app === "codex"
            ? "openai_responses"
            : app === "gemini"
              ? "gemini_native"
              : "anthropic",
      },
      icon:
        app === "codex" ? "openai" : app === "gemini" ? "gemini" : "anthropic",
      iconColor:
        app === "codex" ? "#00A67E" : app === "gemini" ? "#4285F4" : "#D4915D",
      createdAt: Date.now(),
    };

    if (getProviders(app)[id]) {
      updateProvider(app, provider);
    } else {
      addProvider(app, provider);
    }
    synced += 1;
  }
  return synced;
};

export const companyAuthHandlers = [
  http.post(`${TAURI_ENDPOINT}/company_auth_get_state`, () =>
    success(getCompanyAuthState()),
  ),
  http.post(
    `${TAURI_ENDPOINT}/company_auth_login_with_ticket`,
    async ({ request }) => {
      const ticket = await readTicket(request);
      if (ticket === "fs_bad") {
        return HttpResponse.text("401 invalid ticket sk-test-secret", {
          status: 401,
        });
      }

      const nextState = {
        authenticated: true,
        user: {
          display_name: "张三",
          email: "zhangsan@example.com",
        },
        base_url: "https://leharrt.com",
        session_token: "test_session_token_should_not_render",
        api_key: ["fixture", "api", "key", "should", "not", "render"].join(
          "-",
        ),
        catalog: [
          {
            tool: "codex",
            label: "公司号池 Codex",
            protocol: "openai-responses",
            default_model: "gpt-5.5",
            models: ["gpt-5.5", "gpt-5.4"],
            group: "default",
          },
          {
            tool: "claude",
            label: "公司号池 Claude",
            protocol: "anthropic",
            default_model: "claude-opus-4-8",
            models: ["claude-opus-4-8"],
            group: "default",
          },
          {
            tool: "claude",
            label: "GLM",
            protocol: "anthropic",
            default_model: "glm-4.6",
            models: ["glm-4.6"],
            group: "default",
          },
          {
            tool: "claude",
            label: "Grok",
            protocol: "anthropic",
            default_model: "grok-4-1-fast",
            models: ["grok-4-1-fast"],
            group: "default",
          },
          {
            tool: "gemini",
            label: "公司号池 Gemini",
            protocol: "gemini",
            default_model: "gemini-3.5-flash",
            models: ["gemini-3.5-flash"],
            group: "default",
          },
        ],
      };
      setCompanyAuthState(nextState);
      return success(nextState);
    },
  ),
  http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
    success({
      base_url: "https://leharrt.com",
      catalog: [
        {
          tool: "codex",
          label: "刷新后的 Codex",
          protocol: "openai-responses",
          default_model: "gpt-5.4",
          models: ["gpt-5.4"],
          group: "default",
        },
      ],
    }),
  ),
  http.post(`${TAURI_ENDPOINT}/company_auth_sync_providers`, () =>
    success({ synced: syncMockCompanyProviders() }),
  ),
  http.post(`${TAURI_ENDPOINT}/company_auth_logout`, () => {
    setCompanyAuthState({ authenticated: false });
    return success(null);
  }),
  http.post(`${TAURI_ENDPOINT}/company_auth_start_feishu_login`, () =>
    success({ url: "https://leharrt.com/api/provision/feishu/start" }),
  ),
];
