import { describe, expect, it } from "vitest";
import { normalizeCodexCatalogModelsForSave } from "@/components/providers/forms/ProviderForm";
import { mapCodexCatalogModelForForm } from "@/components/providers/forms/hooks/useCodexConfigState";

describe("ProviderForm Codex catalog helpers", () => {
  it("normalizes catalog rows and removes empty or duplicate models", () => {
    expect(
      normalizeCodexCatalogModelsForSave([
        { model: " deepseek-v4-flash ", displayName: " DeepSeek " },
        { model: "deepseek-v4-flash", displayName: "Duplicate" },
        { model: "", displayName: "Empty" },
        { model: "kimi-k2", contextWindow: "128000 tokens" },
      ]),
    ).toEqual([
      { model: "deepseek-v4-flash", displayName: "DeepSeek" },
      { model: "kimi-k2", contextWindow: 128000 },
    ]);
  });

  it("preserves native-profile overrides (parallel tool calls + input modalities + base instructions)", () => {
    expect(
      normalizeCodexCatalogModelsForSave([
        {
          model: "MiniMax-M3",
          displayName: "MiniMax-M3",
          contextWindow: 1000000,
          supportsParallelToolCalls: true,
          inputModalities: ["text", "image"],
          baseInstructions:
            "  You are Codex, a coding agent based on MiniMax-M3.  ",
        },
        // false must be preserved (not dropped as falsy); empty modalities dropped;
        // empty/whitespace baseInstructions dropped
        {
          model: "mimo-v2.5-pro",
          supportsParallelToolCalls: false,
          inputModalities: [],
          baseInstructions: "   ",
        },
      ]),
    ).toEqual([
      {
        model: "MiniMax-M3",
        displayName: "MiniMax-M3",
        contextWindow: 1000000,
        supportsParallelToolCalls: true,
        inputModalities: ["text", "image"],
        baseInstructions: "You are Codex, a coding agent based on MiniMax-M3.",
      },
      { model: "mimo-v2.5-pro", supportsParallelToolCalls: false },
    ]);
  });

  it("preserves per-model reasoning levels and default level", () => {
    expect(
      normalizeCodexCatalogModelsForSave([
        {
          model: "deepseek-v4-flash",
          displayName: "DeepSeek V4 Flash",
          reasoningLevels: ["none", "low", "medium", "high", "xhigh", "max"],
          defaultReasoningLevel: " xhigh ",
        },
        // empty levels / whitespace default are dropped
        {
          model: "plain-model",
          reasoningLevels: [],
          defaultReasoningLevel: "   ",
        },
      ]),
    ).toEqual([
      {
        model: "deepseek-v4-flash",
        displayName: "DeepSeek V4 Flash",
        reasoningLevels: ["none", "low", "medium", "high", "xhigh", "max"],
        defaultReasoningLevel: "xhigh",
      },
      { model: "plain-model" },
    ]);
  });

  it("round-trips reasoning levels through load and save without loss", () => {
    // load→save 回环：加载映射（mapCodexCatalogModelForForm）与保存归一化
    // （normalizeCodexCatalogModelsForSave）各锁半边时，回环丢字段两边都测不出——
    // 而编辑保存丢表会让依赖逐模型档位的功能（zen 钳制）静默失效且 UI 无可察觉。
    const stored = [
      {
        model: "glm-5.2",
        displayName: "GLM 5.2",
        reasoningLevels: ["high", "max"],
      },
      // 手写/旧数据可能是 snake_case，加载侧兼容后保存侧同样要留住
      { model: "deepseek-v4-flash", reasoning_levels: ["low", "high", "max"] },
      { model: "glm-5.1" }, // toggle 型：无表，全程不得凭空造表
    ];

    const roundTripped = normalizeCodexCatalogModelsForSave(
      stored.map(mapCodexCatalogModelForForm),
    );

    expect(roundTripped).toEqual([
      {
        model: "glm-5.2",
        displayName: "GLM 5.2",
        reasoningLevels: ["high", "max"],
      },
      { model: "deepseek-v4-flash", reasoningLevels: ["low", "high", "max"] },
      { model: "glm-5.1" },
    ]);
  });

  it("trims reasoning level values on save", () => {
    // 手编 JSON 里的 " high " 不得原样落库/发给上游。
    expect(
      normalizeCodexCatalogModelsForSave([
        { model: "glm-5.2", reasoningLevels: [" high ", "max"] },
      ]),
    ).toEqual([{ model: "glm-5.2", reasoningLevels: ["high", "max"] }]);
  });

  it("round-trips nested company capabilities without making fallback context explicit", () => {
    const stored = {
      model: "company-codex",
      capabilities: {
        context_window: 128000,
        supports_parallel_tool_calls: false,
        input_modalities: ["text", "image"],
        future_metadata: { enabled: true, limits: [0, 42], optional: null },
        catalog_snapshot: {
          schema_version: 1,
          codex_version: "0.134.0",
          model: {
            slug: "company-codex",
            base_instructions: "Native instructions must survive DB edits.",
            tools: [{ name: "shell", options: { enabled: true } }],
          },
        },
      },
    };

    const loaded = mapCodexCatalogModelForForm(stored);
    expect(loaded).toMatchObject({ capabilities: stored.capabilities });
    expect(loaded.contextWindow).toBe("");

    const [saved] = normalizeCodexCatalogModelsForSave([loaded]);
    expect(saved).toEqual(stored);
    expect(saved).not.toHaveProperty("contextWindow");
    expect(saved).not.toHaveProperty("context_window");
  });

  it.each(["contextWindow", "context_window"])(
    "keeps explicit %s separate from fallback capabilities",
    (contextField) => {
      const capabilities = {
        context_window: 128000,
        reasoning: { enabled: false },
      };
      const loaded = mapCodexCatalogModelForForm({
        model: "company-codex",
        [contextField]: "256000",
        capabilities,
      });

      expect(loaded.contextWindow).toBe("256000");
      expect(normalizeCodexCatalogModelsForSave([loaded])).toEqual([
        { model: "company-codex", contextWindow: 256000, capabilities },
      ]);
    },
  );

  it("preserves an empty capabilities object", () => {
    const stored = { model: "company-codex", capabilities: {} };
    expect(
      normalizeCodexCatalogModelsForSave([mapCodexCatalogModelForForm(stored)]),
    ).toEqual([stored]);
  });

  it.each([undefined, null, false, 128000, "invalid", []])(
    "does not carry malformed capabilities (%j) into form state or saved rows",
    (capabilities) => {
      const loaded = mapCodexCatalogModelForForm({
        model: "company-codex",
        capabilities,
      });
      expect(loaded).not.toHaveProperty("capabilities");
      expect(normalizeCodexCatalogModelsForSave([loaded])).toEqual([
        { model: "company-codex" },
      ]);
    },
  );
});
