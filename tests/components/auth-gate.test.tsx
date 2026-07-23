import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { server } from "../msw/server";
import {
  getProviders,
  resetProviderState,
  setCompanyAuthState,
} from "../msw/state";
import { COMPANY_CATALOG_SELECT_EVENT } from "@/components/auth/companyCatalogEvents";

const TAURI_ENDPOINT = "http://tauri.local";
const GUIDE_ACK_STORAGE_KEY = "token-switch-company-auth-guide-ack-v1";

vi.setConfig({ testTimeout: 10000 });

vi.mock("@/components/providers/ProviderList", () => ({
  ProviderList: ({ appId }: { appId: string }) => (
    <div data-testid="provider-ui" data-app-id={appId}>
      provider ui
    </div>
  ),
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

const writeEvidence = (fileName: string) => {
  const dir = process.env.AUTH_UI_EVIDENCE_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, fileName), document.body.innerHTML, "utf8");
};

const expectNoSecretsRendered = () => {
  const text = document.body.textContent ?? "";
  expect(text).not.toContain("sk-test-secret");
  expect(text).not.toContain("test_session_token");
  expect(text).not.toContain("Authorization: Bearer");
};

const clearGuideAcknowledgement = () => {
  window.localStorage.removeItem(GUIDE_ACK_STORAGE_KEY);
};

const acknowledgeGuide = () => {
  window.localStorage.setItem(GUIDE_ACK_STORAGE_KEY, "1");
};

describe("company auth gate", () => {
  beforeEach(() => {
    cleanup();
    resetProviderState();
    clearGuideAcknowledgement();
  });

  it("shows the login gate before provider UI when unauthenticated", async () => {
    setCompanyAuthState({ authenticated: false });

    await renderApp();

    expect(
      await screen.findByRole("heading", { name: "公司账号登录" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "使用飞书登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it("logs in with a valid fs ticket and shows user and catalog metadata", async () => {
    setCompanyAuthState({ authenticated: false });
    const user = userEvent.setup();

    await renderApp();
    await user.type(
      await screen.findByLabelText("一次性 ticket"),
      "fs_test_ok",
    );
    await user.click(screen.getByRole("button", { name: "提交 ticket" }));

    expect(
      await screen.findByRole("heading", {
        name: "登录成功，开始使用 CC Switch",
      }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "我已了解" }));

    expect(await screen.findByText("张三")).toBeInTheDocument();
    expect(screen.getByText("zhangsan@example.com")).toBeInTheDocument();
    expect(screen.getByText("张卡片")).toBeInTheDocument();
    expect(screen.getByTitle(/公司号池 Codex/)).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
    writeEvidence("task-3-green-auth-shell.html");
  });

  it("hydrates authenticated startup and keeps the provider UI reachable", async () => {
    acknowledgeGuide();
    setCompanyAuthState({
      authenticated: true,
      user: {
        display_name: "李四",
        email: "lisi@example.com",
      },
      catalog: [
        {
          tool: "codex",
          label: "第一目录",
          protocol: "openai-responses",
          default_model: "gpt-5.5",
          models: ["gpt-5.5"],
          group: "default",
        },
        {
          tool: "gemini-cli",
          label: "第二目录",
          protocol: "gemini",
          default_model: "gemini-3.5-flash",
          models: ["gemini-3.5-flash"],
          group: "default",
        },
      ],
    });

    await renderApp();

    expect(await screen.findByText("李四")).toBeInTheDocument();
    expect(screen.getByText("lisi@example.com")).toBeInTheDocument();
    expect(screen.getByTitle(/第一目录/)).toBeInTheDocument();
    expect(screen.getByTitle(/第二目录/)).toBeInTheDocument();
    expect(screen.getByText("张卡片")).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    await waitFor(() => {
      expect(
        Object.values(getProviders("codex")).some(
          (provider) =>
            provider.id.startsWith("company-codex-") &&
            provider.name === "第一目录" &&
            provider.meta?.providerType === "company_auth",
        ),
      ).toBe(true);
    });

    act(() => {
      window.dispatchEvent(
        new CustomEvent(COMPANY_CATALOG_SELECT_EVENT, {
          detail: { tool: "gemini-cli", label: "第二目录" },
        }),
      );
    });
    await waitFor(() => {
      expect(screen.getByTestId("provider-ui")).toHaveAttribute(
        "data-app-id",
        "gemini",
      );
    });
    expectNoSecretsRendered();
  });

  it("keeps the authenticated shell visible at the default Tauri window width", async () => {
    acknowledgeGuide();
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 1000,
    });
    setCompanyAuthState({
      authenticated: true,
      user: {
        display_name: "王五",
        email: "wangwu@example.com",
      },
      catalog: [
        {
          tool: "codex",
          label: "默认目录",
          protocol: "openai-responses",
          default_model: "gpt-5.5",
          models: ["gpt-5.5"],
          group: "default",
        },
      ],
    });

    await renderApp();

    const shell = await screen.findByLabelText("公司认证状态");
    const defaultWindowWrapper = shell.parentElement;
    expect(defaultWindowWrapper).not.toBeNull();
    expect(defaultWindowWrapper).toHaveClass("flex");
    expect(defaultWindowWrapper).not.toHaveClass("hidden");
    expect(defaultWindowWrapper).not.toHaveClass("lg:flex");
    expect(screen.getByText("王五")).toBeInTheDocument();
    expect(screen.getByText("张卡片")).toBeInTheDocument();
    expect(screen.getByTitle(/默认目录/)).toBeInTheDocument();
    expect(screen.getByTitle("查看号池卡片")).toBeInTheDocument();
    expect(screen.getByTitle("刷新目录")).toBeInTheDocument();
    expect(screen.getByTitle("退出登录")).toBeInTheDocument();
  });

  it("shows a Codex quick-start guide after Feishu auth until acknowledged", async () => {
    setCompanyAuthState({
      authenticated: true,
      user: {
        display_name: "赵六",
        email: "zhaoliu@example.com",
      },
      catalog: [
        {
          tool: "codex",
          label: "公司号池 Codex",
          protocol: "openai-responses",
          default_model: "gpt-5.5",
          models: ["gpt-5.5"],
          group: "default",
        },
      ],
    });
    const user = userEvent.setup();

    const app = await renderApp();

    expect(
      await screen.findByRole("heading", {
        name: "登录成功，开始使用 CC Switch",
      }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("1. 点击顶部 Codex 图标").length).toBeGreaterThan(
      0,
    );
    expect(screen.getByText("2. 选择公司号池 Codex")).toBeInTheDocument();
    expect(screen.getByText("3. 点击启用并确认使用中")).toBeInTheDocument();
    expect(
      screen.getByRole("img", { name: "点击顶部工具栏里的 Codex 图标" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("img", { name: "公司号池 Codex 卡片显示使用中" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "我已了解" }));

    expect(await screen.findByText("赵六")).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();

    app.unmount();
    await renderApp();

    expect(
      screen.queryByRole("heading", {
        name: "登录成功，开始使用 CC Switch",
      }),
    ).not.toBeInTheDocument();
    expect(await screen.findByText("赵六")).toBeInTheDocument();
    expect(screen.getByTestId("provider-ui")).toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it.each([
    ["empty ticket", ""],
    ["non-fs ticket", "bad_ticket"],
  ])("keeps malformed input on the gate: %s", async (_name, ticket) => {
    setCompanyAuthState({ authenticated: false });
    const user = userEvent.setup();

    await renderApp();
    const input = await screen.findByLabelText("一次性 ticket");
    if (ticket) {
      await user.type(input, ticket);
    }
    await user.click(screen.getByRole("button", { name: "提交 ticket" }));

    expect(
      await screen.findByText("请输入 fs_ 开头的一次性 ticket。"),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
    expectNoSecretsRendered();
  });

  it.each([
    [
      "401 invalid ticket",
      () =>
        HttpResponse.text("401 invalid ticket sk-test-secret-raw", {
          status: 401,
        }),
      "Ticket 无效或已过期，请重新获取。",
    ],
    [
      "403 forbidden tenant",
      () =>
        HttpResponse.text("403 forbidden test_session_token_raw", {
          status: 403,
        }),
      "当前飞书租户无权限使用公司客户端。",
    ],
    [
      "network failure",
      () => HttpResponse.error(),
      "暂时无法连接认证服务，请检查网络后重试。",
    ],
    [
      "malformed response",
      () =>
        HttpResponse.json({
          authenticated: true,
          user: { display_name: "张三" },
        }),
      "认证服务返回格式异常，请稍后重试。",
    ],
  ])(
    "keeps %s on the login gate with a redacted error",
    async (_name, response, message) => {
      setCompanyAuthState({ authenticated: false });
      server.use(
        http.post(`${TAURI_ENDPOINT}/company_auth_login_with_ticket`, () =>
          response(),
        ),
      );
      const user = userEvent.setup();

      await renderApp();
      await user.type(
        await screen.findByLabelText("一次性 ticket"),
        "fs_test_bad",
      );
      await user.click(screen.getByRole("button", { name: "提交 ticket" }));

      expect(await screen.findByText(message)).toBeInTheDocument();
      expect(screen.queryByTestId("provider-ui")).not.toBeInTheDocument();
      expectNoSecretsRendered();
      writeEvidence("task-3-login-failure.html");
    },
  );
});
