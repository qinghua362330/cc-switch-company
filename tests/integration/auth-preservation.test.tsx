import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppId } from "@/lib/api/types";
import type { Provider } from "@/types";
import { server } from "../msw/server";
import {
  getCurrentProviderId,
  getLiveProviderIds,
  getProviders,
  resetProviderState,
  setCompanyAuthState,
} from "../msw/state";

const TAURI_ENDPOINT = "http://tauri.local";
const GUIDE_ACK_STORAGE_KEY = "token-switch-company-auth-guide-ack-v1";
const APP_IDS = [
  "claude",
  "claude-desktop",
  "codex",
  "gemini",
  "opencode",
  "openclaw",
  "hermes",
] as const satisfies readonly AppId[];
const LIVE_APP_IDS = ["opencode", "openclaw", "hermes"] as const;
const SECRET_KEY = ["sk-test", "secret"].join("-");
const SESSION_TOKEN = ["test_session", "token"].join("_");
const AUTH_HEADER_PREFIX = ["Authorization:", "Bearer"].join(" ");
const AUTH_HEADER = `${AUTH_HEADER_PREFIX} ${SESSION_TOKEN}`;
const GENERIC_SESSION_NAME = ["session", "token"].join("_");
const GENERIC_SESSION_VALUE = ["runtime", "session", "credential"].join("-");
const GENERIC_SESSION_PAIR = `${GENERIC_SESSION_NAME}=${GENERIC_SESSION_VALUE}`;
const GENERIC_API_NAME = ["api", "key"].join("_");
const GENERIC_API_VALUE = ["runtime", "api", "credential"].join("-");
const GENERIC_API_PAIR = `${GENERIC_API_NAME}: ${GENERIC_API_VALUE}`;
const GENERIC_OPENAI_KEY = ["sk", "runtime", "generic", "credential"].join("-");
const GENERIC_AUTH_HEADER = `${AUTH_HEADER_PREFIX} ${GENERIC_SESSION_VALUE}`;

vi.setConfig({ testTimeout: 10000 });

type LiveAppId = (typeof LIVE_APP_IDS)[number];
type ProviderSnapshot = {
  readonly providers: readonly (readonly [AppId, Record<string, Provider>])[];
  readonly current: readonly (readonly [AppId, string])[];
  readonly live: readonly (readonly [LiveAppId, readonly string[]])[];
};

vi.mock("@/components/providers/ProviderList", () => ({
  ProviderList: () => <div data-testid="provider-ui">provider ui</div>,
}));

vi.mock("@/components/UpdateBadge", () => ({
  UpdateBadge: ({ onClick }: { readonly onClick: () => void }) => (
    <button type="button" onClick={onClick}>
      update-badge
    </button>
  ),
}));

const renderApp = async () => {
  const { default: App } = await import("@/App");
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
      },
    },
  });

  return render(
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>,
  );
};

const snapshotProviders = (): ProviderSnapshot => {
  const providers = APP_IDS.map((appId) => {
    const appProviders = Object.fromEntries(
      Object.entries(getProviders(appId)).filter(
        ([, provider]) => provider.meta?.providerType !== "company_auth",
      ),
    );
    return [appId, appProviders] as const;
  });
  const current = APP_IDS.map(
    (appId) => [appId, getCurrentProviderId(appId)] as const,
  );
  const live = LIVE_APP_IDS.map(
    (appId) => [appId, getLiveProviderIds(appId)] as const,
  );

  return { providers, current, live };
};

const expectProviderSnapshotPreserved = (before: ProviderSnapshot) => {
  expect(snapshotProviders()).toEqual(before);
};

const expectUiVisibleSecretsRedacted = () => {
  const text = document.body.textContent ?? "";
  expect(text).not.toContain(SECRET_KEY);
  expect(text).not.toContain(SESSION_TOKEN);
  expect(text).not.toContain(AUTH_HEADER_PREFIX);
  expect(text).not.toContain(AUTH_HEADER);
  expect(text).not.toContain(GENERIC_SESSION_NAME);
  expect(text).not.toContain(GENERIC_SESSION_VALUE);
  expect(text).not.toContain(GENERIC_SESSION_PAIR);
  expect(text).not.toContain(GENERIC_API_NAME);
  expect(text).not.toContain(GENERIC_API_VALUE);
  expect(text).not.toContain(GENERIC_API_PAIR);
  expect(text).not.toContain(GENERIC_OPENAI_KEY);
  expect(text).not.toContain(GENERIC_AUTH_HEADER);
};

describe("company auth preservation and redaction", () => {
  beforeEach(() => {
    cleanup();
    resetProviderState();
    window.localStorage.setItem(GUIDE_ACK_STORAGE_KEY, "1");
  });

  it("preserves providers, current provider, and live provider ids across login refresh and logout", async () => {
    setCompanyAuthState({ authenticated: false });
    const before = snapshotProviders();
    const user = userEvent.setup();

    await renderApp();
    await user.type(
      await screen.findByLabelText("一次性 ticket"),
      "fs_test_ok",
    );
    await user.click(screen.getByRole("button", { name: "提交 ticket" }));
    expect(await screen.findByTestId("provider-ui")).toBeInTheDocument();
    expectProviderSnapshotPreserved(before);

    fireEvent.click(screen.getByTitle("刷新目录"));
    expect(await screen.findByTitle(/刷新后的 Codex/)).toBeInTheDocument();
    expectProviderSnapshotPreserved(before);

    fireEvent.click(screen.getByTitle("退出登录"));
    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expectProviderSnapshotPreserved(before);
    expectUiVisibleSecretsRedacted();
  });

  it("redacts secret-like backend values before they reach rendered auth UI", async () => {
    setCompanyAuthState({
      authenticated: true,
      user: {
        display_name: `Employee ${SECRET_KEY}`,
        email: `${SESSION_TOKEN}@example.com`,
      },
      base_url: "https://leharrt.com",
      catalog: [
        {
          tool: "codex",
          label: `Visible ${AUTH_HEADER}`,
          protocol: "openai-responses",
          default_model: "gpt-5.5",
          models: [`gpt-5.5 ${SECRET_KEY}`],
          group: "default",
        },
      ],
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        Response.json({
          base_url: "https://leharrt.com",
          catalog: [
            {
              tool: "codex",
              label: `Refresh ${AUTH_HEADER}`,
              protocol: "openai-responses",
              default_model: `model-${SESSION_TOKEN}`,
              models: [`model-${SECRET_KEY}`],
              group: "default",
            },
          ],
        }),
      ),
    );
    await renderApp();
    expect(await screen.findByLabelText("公司认证状态")).toBeInTheDocument();
    expectUiVisibleSecretsRedacted();

    fireEvent.click(screen.getByTitle("刷新目录"));
    expect(await screen.findByTitle(/Refresh/)).toBeInTheDocument();
    expectUiVisibleSecretsRedacted();
  });

  it("redacts generic backend-controlled session and api key strings before rendering", async () => {
    setCompanyAuthState({
      authenticated: true,
      user: {
        display_name: `Employee ${GENERIC_SESSION_PAIR}`,
        email: `${GENERIC_API_PAIR}@example.com`,
      },
      base_url: "https://leharrt.com",
      catalog: [
        {
          tool: "codex",
          label: `Visible ${GENERIC_AUTH_HEADER}`,
          protocol: "openai-responses",
          default_model: `model-${GENERIC_OPENAI_KEY}`,
          models: [`gpt-5.5 ${GENERIC_API_PAIR}`],
          group: `default ${GENERIC_SESSION_PAIR}`,
        },
      ],
    });
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        Response.json({
          base_url: "https://leharrt.com",
          catalog: [
            {
              tool: "codex",
              label: `Refresh ${GENERIC_SESSION_PAIR}`,
              protocol: "openai-responses",
              default_model: `model-${GENERIC_API_PAIR}`,
              models: [`model-${GENERIC_OPENAI_KEY}`],
              group: `default ${GENERIC_AUTH_HEADER}`,
            },
          ],
        }),
      ),
    );

    await renderApp();
    expect(await screen.findByLabelText("公司认证状态")).toBeInTheDocument();
    expectUiVisibleSecretsRedacted();

    fireEvent.click(screen.getByTitle("刷新目录"));
    expect(await screen.findByTitle(/Refresh/)).toBeInTheDocument();
    expectUiVisibleSecretsRedacted();
  });
});
