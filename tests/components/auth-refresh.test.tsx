import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CompanyAuthShell } from "@/components/auth/CompanyAuthShell";
import { server } from "../msw/server";
import { resetProviderState } from "../msw/state";

const TAURI_ENDPOINT = "http://tauri.local";
const GUIDE_ACK_STORAGE_KEY = "token-switch-company-auth-guide-ack-v1";

vi.setConfig({ testTimeout: 10000 });

vi.mock("@/components/providers/ProviderList", () => ({
  ProviderList: () => <div data-testid="provider-ui">provider ui</div>,
}));

vi.mock("@/components/UpdateBadge", () => ({
  UpdateBadge: ({ onClick }: { onClick: () => void }) => (
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

const expectNoSecretsRendered = () => {
  const text = document.body.textContent ?? "";
  expect(text).not.toContain("sk-test-secret");
  expect(text).not.toContain("test_session_token");
  expect(text).not.toContain("Authorization: Bearer");
};

describe("company auth refresh lifecycle", () => {
  beforeEach(() => {
    cleanup();
    resetProviderState();
    window.localStorage.setItem(GUIDE_ACK_STORAGE_KEY, "1");
  });

  it("refreshes catalog metadata without leaving the authenticated shell", async () => {
    await renderApp();
    fireEvent.click(await screen.findByTitle("刷新目录"));

    expect(await screen.findByTitle(/刷新后的 Codex/)).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("keeps cached catalog metadata and shows a redacted error when refresh returns 5xx", async () => {
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        HttpResponse.text("500 upstream failed sk-test-secret-raw", {
          status: 500,
        }),
      ),
    );

    await renderApp();
    expect(await screen.findByTitle(/公司号池 Codex/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("刷新目录"));

    expect(
      await screen.findByText("登录失败，请稍后重试。"),
    ).toBeInTheDocument();
    expect(screen.getAllByTitle(/公司号池 Codex/).length).toBeGreaterThan(0);
    expect(screen.queryByText("sk-test-secret-raw")).not.toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("keeps refresh error text visible at the default 1000px browser width", () => {
    render(
      <CompanyAuthShell
        state={{
          authenticated: true,
          user: {
            displayName: "Task6员工",
            email: "task6@example.com",
          },
          catalog: [
            {
              tool: "codex",
              label: "公司号池 Codex",
              protocol: "openai-responses",
              defaultModel: "gpt-5.5",
              models: ["gpt-5.5"],
              group: "default",
            },
          ],
          baseUrl: "https://leharrt.com",
        }}
        error="登录失败，请稍后重试。"
        isRefreshing={false}
        isLoggingOut={false}
        onRefresh={vi.fn()}
        onLogout={vi.fn()}
      />,
    );

    const error = screen.getByText("登录失败，请稍后重试。");
    expect(error).toHaveClass("md:inline");
    expect(error).not.toHaveClass("lg:inline");
  });

  it("keeps cached catalog metadata and shows a redacted error when refresh has a network error", async () => {
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        HttpResponse.error(),
      ),
    );

    await renderApp();
    expect(await screen.findByTitle(/公司号池 Codex/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("刷新目录"));

    expect(
      await screen.findByText("暂时无法连接认证服务，请检查网络后重试。"),
    ).toBeInTheDocument();
    expect(screen.getAllByTitle(/公司号池 Codex/).length).toBeGreaterThan(0);
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("keeps cached catalog metadata and shows a redacted error when refresh response is malformed", async () => {
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        HttpResponse.json({
          base_url: "https://leharrt.com",
          catalog: [
            {
              tool: "codex",
              label: "缺少模型字段",
              protocol: "openai-responses",
              default_model: "gpt-5.4",
              group: "default",
            },
          ],
        }),
      ),
    );

    await renderApp();
    expect(await screen.findByTitle(/公司号池 Codex/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("刷新目录"));

    expect(
      await screen.findByText("目录服务返回格式异常，请稍后重试。"),
    ).toBeInTheDocument();
    expect(screen.getAllByTitle(/公司号池 Codex/).length).toBeGreaterThan(0);
    expect(screen.queryByText("缺少模型字段")).not.toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("clears local auth on catalog 401 and stays on the login gate after restart", async () => {
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_refresh_catalog`, () =>
        HttpResponse.text("401 expired test_session_token_raw", {
          status: 401,
        }),
      ),
    );

    const app = await renderApp();
    expect(await screen.findByTitle(/公司号池 Codex/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("刷新目录"));

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expect(
      screen.queryByText("test_session_token_raw"),
    ).not.toBeInTheDocument();

    app.unmount();
    await renderApp();

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("logs out and returns to the gate", async () => {
    await renderApp();
    fireEvent.click(await screen.findByTitle("退出登录"));

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("keeps the authenticated shell visible and shows a generic error when logout fails", async () => {
    server.use(
      http.post(`${TAURI_ENDPOINT}/company_auth_logout`, () =>
        HttpResponse.text(
          "500 keychain delete failed Authorization: Bearer test_session_token_raw sk-test-secret-raw",
          { status: 500 },
        ),
      ),
    );

    await renderApp();
    expect(await screen.findByTitle(/公司号池 Codex/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("退出登录"));

    expect(
      await screen.findByText("退出登录失败，请稍后重试。"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("公司认证状态")).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "公司账号登录" }),
    ).not.toBeInTheDocument();
    expect(screen.getByTitle("退出登录")).not.toBeDisabled();
    expectNoSecretsRendered();
  });

  it("stays on the login gate after logout and restart", async () => {
    const app = await renderApp();
    fireEvent.click(await screen.findByTitle("退出登录"));

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();

    app.unmount();
    await renderApp();

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expectNoSecretsRendered();
  });
});
