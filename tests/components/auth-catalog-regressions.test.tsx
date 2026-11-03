import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CompanyAuthGate } from "@/components/auth/CompanyAuthGate";
import { normalizeCompanyAuthState } from "@/lib/api/companyAuth";
import { providersApi } from "@/lib/api";
import { server } from "../msw/server";
import { setCompanyAuthState } from "../msw/companyAuthState";
import { companyAuthHandlers } from "../msw/companyAuthHandlers";
import { getProviders } from "../msw/state";

const poll = vi.hoisted(() => ({ daily: async () => {} }));
vi.mock("@/hooks/useCompanyCatalogPoll", () => ({
  useCompanyCatalogPoll: ({
    onDailyRefresh,
  }: {
    onDailyRefresh: () => Promise<void>;
  }) => {
    poll.daily = onDailyRefresh;
  },
}));
vi.mock("@/lib/userAttention", () => ({
  showCompanyCatalogUpdatedDialog: vi.fn().mockResolvedValue(undefined),
}));

const endpoint = "http://tauri.local";
const entry = {
  tool: "codex",
  label: "Company Codex",
  protocol: "openai-responses",
  default_model: "gpt-5.5",
  models: ["gpt-5.5"],
  group: "default",
};
const metadata = {
  authenticated: true,
  user: { display_name: "Employee", email: "test@example.com" },
  base_url: "https://example.test",
  catalog: [entry],
};

function builderCapabilities() {
  return {
    input_modalities: ["text"],
    max_input_tokens: 200000,
    max_output_tokens: 32000,
    source: {
      fields: { context_window: "models.dev" },
      codex_bundled: {
        kind: "codex-bundled",
        sha256: "a".repeat(64),
        codex_version: "0.134.0",
      },
      models_dev: {
        kind: "models.dev",
        sha256: "b".repeat(64),
        provider: "openai",
        model_id: "gpt-5.5",
      },
    },
    catalog_snapshot: {
      schema_version: 1,
      codex_version: "0.134.0",
      model: {
        slug: "gpt-5.5",
        base_instructions: "private native instructions",
      },
      source: { kind: "codex-bundled", sha256: "a".repeat(64) },
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function renderGate() {
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <CompanyAuthGate>
        {({ shell }) => (
          <>
            <div data-testid="app-content">App</div>
            {shell}
          </>
        )}
      </CompanyAuthGate>
    </QueryClientProvider>,
  );
  await screen.findByTestId("app-content");
  await waitFor(() => expect(screen.getByTitle("刷新目录")).toBeEnabled());
}

describe("company catalog transactional refresh", () => {
  let sync: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("token-switch-company-auth-guide-ack-v1", "1");
    setCompanyAuthState(metadata);
    sync = vi.fn(() => HttpResponse.json({ synced: 1 }));
    server.use(
      http.post(`${endpoint}/company_auth_sync_providers`, () => sync()),
    );
  });

  it.each(["manual", "daily"])(
    "unmounts app content for a real Rust unauthorized error during %s refresh",
    async (mode) => {
      server.use(
        http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
          HttpResponse.text("unauthorized: 登录已失效，请重新登录。", {
            status: 401,
          }),
        ),
      );
      await renderGate();
      await waitFor(() => expect(sync).toHaveBeenCalledOnce());
      if (mode === "manual") fireEvent.click(screen.getByTitle("刷新目录"));
      else
        await act(async () => {
          await poll.daily().catch(() => {});
        });
      expect(
        await screen.findByRole("heading", { name: "公司账号登录" }),
      ).toBeInTheDocument();
      expect(screen.queryByTestId("app-content")).not.toBeInTheDocument();
    },
  );

  it.each([
    {
      model_capabilities: {
        "gpt-5.5": { supported_reasoning_levels: ["high", "ultra"] },
      },
    },
    { models: ["gpt-5.5", "gpt-5.6"] },
    { group: "premium" },
  ])("syncs a daily catalog-only change: %j", async (change) => {
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
        HttpResponse.json({
          base_url: metadata.base_url,
          catalog: [{ ...entry, ...change }],
        }),
      ),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    await act(async () => {
      await poll.daily();
    });
    expect(sync).toHaveBeenCalledTimes(2);
  });

  it.each([
    "source.fields",
    "source.codex_bundled",
    "source.models_dev",
    "snapshot.source",
    "input_modalities",
    "max_input_tokens",
    "max_output_tokens",
  ])(
    "syncs real builder wire metadata when only %s changes",
    async (change) => {
      const caps = builderCapabilities();
      const next = builderCapabilities();
      switch (change) {
        case "source.fields":
          next.source.fields.context_window = "codex-bundled";
          break;
        case "source.codex_bundled":
          next.source.codex_bundled.sha256 = "c".repeat(64);
          break;
        case "source.models_dev":
          next.source.models_dev.sha256 = "c".repeat(64);
          break;
        case "snapshot.source":
          next.catalog_snapshot.source.sha256 = "c".repeat(64);
          break;
        case "input_modalities":
          next.input_modalities.push("image");
          break;
        case "max_input_tokens":
          next.max_input_tokens = 300000;
          break;
        case "max_output_tokens":
          next.max_output_tokens = 64000;
          break;
      }
      setCompanyAuthState({
        ...metadata,
        catalog: [{ ...entry, model_capabilities: { "gpt-5.5": caps } }],
      });
      server.use(
        http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
          HttpResponse.json({
            base_url: metadata.base_url,
            catalog: [{ ...entry, model_capabilities: { "gpt-5.5": next } }],
          }),
        ),
      );
      await renderGate();
      await waitFor(() => expect(sync).toHaveBeenCalledOnce());
      await act(async () => {
        await poll.daily();
      });
      expect(sync).toHaveBeenCalledTimes(2);
      await act(async () => {
        await poll.daily();
      });
      expect(sync).toHaveBeenCalledTimes(2);
      expect(document.body.innerHTML).not.toContain(
        "private native instructions",
      );
    },
  );

  it.each(["native row", "version", "digest", "source"])(
    "syncs a snapshot-only %s change without exposing native instructions",
    async (change) => {
      const snapshot = {
        schema_version: 1,
        codex_version: "0.134.0",
        digest: "sha256:abc123",
        model: {
          slug: "gpt-5.5",
          base_instructions: "private native instructions",
        },
      };
      const caps = {
        context_window: 400000,
        catalog_snapshot: snapshot,
        source: { version: "v1", fingerprint: "sha256:111" },
      };
      setCompanyAuthState({
        ...metadata,
        catalog: [{ ...entry, model_capabilities: { "gpt-5.5": caps } }],
      });
      const next = {
        ...caps,
        catalog_snapshot: {
          ...snapshot,
          ...(change === "native row"
            ? {
                model: {
                  ...snapshot.model,
                  base_instructions: "changed private native instructions",
                },
              }
            : {}),
          ...(change === "version" ? { codex_version: "0.135.0" } : {}),
          ...(change === "digest" ? { digest: "sha256:def456" } : {}),
        },
        ...(change === "source"
          ? { source: { version: "v2", fingerprint: "sha256:222" } }
          : {}),
      };
      server.use(
        http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
          HttpResponse.json({
            base_url: metadata.base_url,
            catalog: [{ ...entry, model_capabilities: { "gpt-5.5": next } }],
          }),
        ),
      );
      await renderGate();
      await waitFor(() => expect(sync).toHaveBeenCalledOnce());
      await act(async () => {
        await poll.daily();
      });
      expect(sync).toHaveBeenCalledTimes(2);
      // An unchanged snapshot must not repeatedly sync after publication.
      await act(async () => {
        await poll.daily();
      });
      expect(sync).toHaveBeenCalledTimes(2);
      expect(document.body.innerHTML).not.toContain(
        "private native instructions",
      );
    },
  );

  it("syncs a base URL change without changing the models", async () => {
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
        HttpResponse.json({
          base_url: "https://new.example.test",
          catalog: [entry],
        }),
      ),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    await act(async () => {
      await poll.daily();
    });
    expect(sync).toHaveBeenCalledTimes(2);
  });

  it("keeps a refresh error visible until a queued retry actually succeeds", async () => {
    const retry = deferred<Response>();
    let calls = 0;
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () => {
        calls += 1;
        return calls === 1 ? HttpResponse.error() : retry.promise;
      }),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByTitle("刷新目录"));
    const message = "暂时无法连接认证服务，请检查网络后重试。";
    await screen.findByText(message);
    let pending!: Promise<void>;
    await act(async () => {
      pending = poll.daily();
      await waitFor(() => expect(calls).toBe(2));
    });
    expect(screen.getByText(message)).toBeInTheDocument();
    await act(async () => {
      retry.resolve(
        HttpResponse.json({ base_url: metadata.base_url, catalog: [entry] }),
      );
      await pending;
    });
    expect(screen.queryByText(message)).not.toBeInTheDocument();
  });

  it("rejects a failed daily sync and retries the same catalog before confirming it", async () => {
    const next = {
      base_url: metadata.base_url,
      catalog: [{ ...entry, label: "Updated Codex" }],
    };
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
        HttpResponse.json(next),
      ),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    sync.mockReturnValueOnce(
      HttpResponse.text("server: sync failed", { status: 500 }),
    );
    await act(async () => {
      await expect(poll.daily()).rejects.toThrow();
    });
    expect(screen.getByTitle(/Company Codex/)).toBeInTheDocument();
    await act(async () => {
      await poll.daily();
    });
    expect(sync).toHaveBeenCalledTimes(3);
    expect(screen.getByTitle(/Updated Codex/)).toBeInTheDocument();
  });

  it("retries unchanged metadata when initial provider sync failed", async () => {
    sync.mockReturnValueOnce(
      HttpResponse.text("server: sync failed", { status: 500 }),
    );
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () =>
        HttpResponse.json({ base_url: metadata.base_url, catalog: [entry] }),
      ),
    );
    await renderGate();
    await screen.findByText("认证服务暂时不可用，请稍后重试。");
    await act(async () => {
      await poll.daily();
    });
    expect(sync).toHaveBeenCalledTimes(2);
  });

  it("serializes daily and manual requests so a slow older response cannot replace the newer catalog", async () => {
    const first = deferred<Response>();
    const refresh = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockReturnValue(
        HttpResponse.json({
          base_url: metadata.base_url,
          catalog: [{ ...entry, label: "Newer Codex" }],
        }),
      );
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () => refresh()),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    let daily!: Promise<void>;
    act(() => {
      daily = poll.daily();
    });
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
    await act(async () => {
      fireEvent.click(screen.getByTitle("刷新目录"));
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(refresh).toHaveBeenCalledOnce();
    await act(async () => {
      first.resolve(
        HttpResponse.json({
          base_url: metadata.base_url,
          catalog: [{ ...entry, label: "Older Codex" }],
        }),
      );
      await daily;
    });
    expect(await screen.findByTitle(/Newer Codex/)).toBeInTheDocument();
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("invalidates an old refresh at logout and queues a new login after logout", async () => {
    const old = deferred<Response>();
    const refresh = vi.fn(() => old.promise);
    const operations: string[] = [];
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () => refresh()),
      http.post(`${endpoint}/company_auth_logout`, () => {
        operations.push("logout");
        setCompanyAuthState({ authenticated: false });
        return HttpResponse.json(null);
      }),
      http.post(`${endpoint}/company_auth_login_with_ticket`, () => {
        operations.push("login");
        setCompanyAuthState(metadata);
        return HttpResponse.json(metadata);
      }),
    );
    await renderGate();
    await waitFor(() => expect(sync).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByTitle("刷新目录"));
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByTitle("退出登录"));
    const input = await screen.findByLabelText("一次性 ticket");
    fireEvent.change(input, { target: { value: "fs_new" } });
    fireEvent.click(screen.getByRole("button", { name: "提交 ticket" }));
    expect(operations).toEqual([]);
    await act(async () => {
      old.resolve(
        HttpResponse.json({
          base_url: metadata.base_url,
          catalog: [{ ...entry, label: "Stale Codex" }],
        }),
      );
    });
    await screen.findByTestId("app-content");
    await waitFor(() => expect(sync).toHaveBeenCalledTimes(2));
    expect(operations).toEqual(["logout", "login"]);
    expect(screen.queryByTitle(/Stale Codex/)).not.toBeInTheDocument();
  });

  it.each(["mcode", "grokbuild"] as const)(
    "MSW preserves providers for the %s app",
    async (app) => {
      const { addProvider } = await import("../msw/state");
      addProvider(app, { id: "fixture", name: "Fixture", settingsConfig: {} });
      expect(await providersApi.getAll(app)).toHaveProperty("fixture");
    },
  );

  it("syncs all production capability fields end to end, including a same-model capability update", async () => {
    server.use(...companyAuthHandlers);
    const caps = {
      supported_reasoning_levels: [
        { effort: "high", description: "High" },
        { effort: "ultra", description: "Ultra" },
      ],
      default_reasoning_level: "high",
      additional_speed_tiers: ["fast"],
      service_tiers: [{ id: "priority", name: "Fast", description: "Pool" }],
      context_window: 400000,
    };
    let latest = caps;
    server.use(
      http.post(`${endpoint}/company_auth_refresh_catalog`, () => {
        const next = {
          ...metadata,
          catalog: [{ ...entry, model_capabilities: { "gpt-5.5": latest } }],
        };
        setCompanyAuthState(next);
        return HttpResponse.json(next);
      }),
    );
    await renderGate();
    await act(async () => {
      await poll.daily();
    });
    const company = () =>
      Object.values(getProviders("codex")).find(
        (provider) => provider.name === entry.label,
      )!;
    expect(company().settingsConfig.modelCatalog).toEqual({
      models: [
        { model: "gpt-5.5", displayName: "gpt-5.5", capabilities: caps },
      ],
    });
    latest = { ...caps, context_window: 1000000 };
    await act(async () => {
      await poll.daily();
    });
    expect(company().settingsConfig.modelCatalog).toEqual({
      models: [
        { model: "gpt-5.5", displayName: "gpt-5.5", capabilities: latest },
      ],
    });
    expect(document.body.innerHTML).not.toContain(
      "sk-test-secret-should-not-render",
    );
  });

  it("emits a credential-free update trigger after login even when provider sync must retry", async () => {
    setCompanyAuthState({ authenticated: false });
    sync.mockReturnValueOnce(
      HttpResponse.text("server: sync failed", { status: 500 }),
    );
    const listener = vi.fn();
    window.addEventListener("company-auth-login", listener);
    try {
      render(
        <QueryClientProvider client={new QueryClient()}>
          <CompanyAuthGate>
            {({ shell }) => (
              <>
                {shell}
                <div data-testid="app-content">App</div>
              </>
            )}
          </CompanyAuthGate>
        </QueryClientProvider>,
      );
      fireEvent.change(await screen.findByLabelText("一次性 ticket"), {
        target: { value: "fs_ok" },
      });
      fireEvent.click(screen.getByRole("button", { name: "提交 ticket" }));
      await screen.findByTestId("app-content");
      await screen.findByText("认证服务暂时不可用，请稍后重试。");
      expect(listener).toHaveBeenCalledOnce();
      expect(listener.mock.calls[0][0]).not.toHaveProperty("detail");
      expect(document.body.innerHTML).not.toContain(
        "test_session_token_should_not_render",
      );
    } finally {
      window.removeEventListener("company-auth-login", listener);
    }
  });
});

describe("company capability DTO", () => {
  it("preserves safe capability changes with snake/camel compatibility and drops credentials", () => {
    const snake = normalizeCompanyAuthState({
      ...metadata,
      session_token: "opaque-secret",
      catalog: [
        {
          ...entry,
          model_capabilities: {
            "gpt-5.5": {
              context_window: 1000000,
              supported_reasoning_levels: ["high", "ultra"],
              api_key: "opaque-secret",
            },
          },
        },
      ],
    });
    const camel = normalizeCompanyAuthState({
      ...metadata,
      catalog: [
        {
          ...entry,
          modelCapabilities: {
            "gpt-5.5": {
              contextWindow: 1000000,
              supportedReasoningLevels: ["high", "ultra"],
            },
          },
        },
      ],
    });
    expect(snake).toEqual(camel);
    expect(snake.authenticated && snake.catalog[0]).toHaveProperty(
      "modelCapabilities",
      {
        "gpt-5.5": {
          context_window: 1000000,
          supported_reasoning_levels: ["high", "ultra"],
        },
      },
    );
    expect(JSON.stringify(snake)).not.toContain("opaque-secret");
  });

  it("allowlists real builder provenance without exposing arbitrary nested fields", () => {
    const caps = builderCapabilities();
    const normalize = (capabilities: Record<string, unknown>) => {
      const state = normalizeCompanyAuthState({
        ...metadata,
        catalog: [
          { ...entry, model_capabilities: { "gpt-5.5": capabilities } },
        ],
      });
      return state.authenticated
        ? state.catalog[0].modelCapabilities?.["gpt-5.5"]
        : undefined;
    };
    const normalized = normalize({
      ...caps,
      source: {
        ...caps.source,
        fields: {
          ...caps.source.fields,
          instructions: "private",
          max_output_tokens: "Bearer credential",
        },
        codex_bundled: {
          ...caps.source.codex_bundled,
          instructions: "private",
        },
        models_dev: { ...caps.source.models_dev, api_key: "private" },
        unknown: { prompt: "private" },
      },
      catalog_snapshot: {
        ...caps.catalog_snapshot,
        source: { ...caps.catalog_snapshot.source, prompt: "private" },
      },
    });
    expect(normalized?.source).toEqual(caps.source);
    expect(normalized?.catalog_snapshot?.source).toEqual(
      caps.catalog_snapshot.source,
    );
    expect(normalized).toMatchObject({
      input_modalities: ["text"],
      max_input_tokens: 200000,
      max_output_tokens: 32000,
    });
    expect(JSON.stringify(normalized)).not.toContain("private");
    expect(JSON.stringify(normalized)).not.toContain("instructions");
    const { input_modalities, max_input_tokens, max_output_tokens, ...rest } =
      caps;
    expect(
      JSON.stringify(
        normalize({
          ...rest,
          inputModalities: input_modalities,
          maxInputTokens: max_input_tokens,
          maxOutputTokens: max_output_tokens,
          source: {
            models_dev: {
              model_id: "gpt-5.5",
              provider: "openai",
              sha256: "b".repeat(64),
              kind: "models.dev",
            },
            codex_bundled: {
              codex_version: "0.134.0",
              sha256: "a".repeat(64),
              kind: "codex-bundled",
            },
            fields: { context_window: "models.dev" },
          },
        }),
      ),
    ).toBe(JSON.stringify(normalized));
    expect(
      normalize({
        input_modalities: ["text", "Bearer credential", { prompt: "private" }],
        max_input_tokens: -1,
        max_output_tokens: "32000",
        source: {
          codex_bundled: { sha256: "not-a-sha256", prompt: "private" },
          models_dev: [],
        },
      }),
    ).toEqual({ input_modalities: ["text"] });
  });

  it("retains only safe snapshot/source descriptors with stable alias and key ordering", () => {
    const normalize = (caps: Record<string, unknown>) =>
      normalizeCompanyAuthState({
        ...metadata,
        catalog: [{ ...entry, model_capabilities: { "gpt-5.5": caps } }],
      });
    const snake = normalize({
      catalog_snapshot: {
        schema_version: 1,
        codex_version: "0.134.0",
        digest: "sha256:123abc",
        model: {
          slug: "gpt-5.5",
          base_instructions: "private native instructions",
          tools: [{ name: "shell", options: { enabled: true, limit: 42 } }],
        },
      },
      source: {
        codex_version: "0.134.0",
        fingerprint: "sha256:abcdef",
        api_key: ["private", "credential"].join(" "),
        instructions: "do not expose",
      },
      provenance: { version: "v1", digest: "sha256:987654" },
    });
    const camel = normalize({
      provenance: { digest: "sha256:987654", version: "v1" },
      source: { fingerprint: "sha256:abcdef", codexVersion: "0.134.0" },
      catalogSnapshot: {
        model: {
          tools: [{ options: { limit: 42, enabled: true }, name: "shell" }],
          base_instructions: "private native instructions",
          slug: "gpt-5.5",
        },
        digest: "sha256:123abc",
        codexVersion: "0.134.0",
        schemaVersion: 1,
      },
    });
    expect(JSON.stringify(snake)).toBe(JSON.stringify(camel));
    expect(
      snake.authenticated &&
        snake.catalog[0].modelCapabilities?.["gpt-5.5"].catalog_snapshot,
    ).toEqual({
      schema_version: 1,
      codex_version: "0.134.0",
      model_slug: "gpt-5.5",
      digest: "sha256:123abc",
      fingerprint: expect.stringMatching(/^fnv1a64:[0-9a-f]{16}$/),
    });
    for (const text of [
      "private native instructions",
      "private credential",
      "do not expose",
      "base_instructions",
      '"tools"',
    ]) {
      expect(JSON.stringify(snake)).not.toContain(text);
    }
  });

  it.each([
    null,
    [],
    { schema_version: 2 },
    {
      schema_version: 1,
      codex_version: "instructions with spaces",
      model: { slug: "gpt-5.5" },
    },
  ])("ignores malformed snapshot descriptors: %j", (catalog_snapshot) => {
    const state = normalizeCompanyAuthState({
      ...metadata,
      catalog: [
        {
          ...entry,
          model_capabilities: {
            "gpt-5.5": {
              catalog_snapshot,
              source: {
                version: "sk-secret",
                fingerprint: "Bearer credential",
                prompt: "instructions",
              },
            },
          },
        },
      ],
    });
    expect(state.authenticated && state.catalog[0].modelCapabilities).toEqual({
      "gpt-5.5": {},
    });
  });
});
