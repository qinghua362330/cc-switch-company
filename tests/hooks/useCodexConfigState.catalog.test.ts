import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useCodexConfigState } from "@/components/providers/forms/hooks/useCodexConfigState";

const makeInitialData = (capabilities: Record<string, unknown>) => ({
  settingsConfig: {
    auth: {},
    config: 'model = "company-codex"\n',
    modelCatalog: {
      models: [{ model: "company-codex", capabilities }],
    },
  },
});

describe("useCodexConfigState company catalog capabilities", () => {
  it("loads fallback capabilities without filling the explicit context window", () => {
    const capabilities = {
      context_window: 128000,
      input_modalities: ["text", "image"],
      reasoning: { enabled: true },
    };
    const initialData = makeInitialData(capabilities);
    const { result } = renderHook(() => useCodexConfigState({ initialData }));

    expect(result.current.codexCatalogModels).toEqual([
      {
        model: "company-codex",
        displayName: "",
        contextWindow: "",
        capabilities,
      },
    ]);
  });

  it("refreshes hidden capabilities when switching providers with identical visible fields", () => {
    const { result, rerender } = renderHook(
      ({ initialData }) => useCodexConfigState({ initialData }),
      {
        initialProps: {
          initialData: makeInitialData({ context_window: 128000 }),
        },
      },
    );
    const capabilities = {
      context_window: 256000,
      supports_parallel_tool_calls: false,
    };
    rerender({ initialData: makeInitialData(capabilities) });

    expect(result.current.codexCatalogModels[0].capabilities).toEqual(
      capabilities,
    );
    expect(result.current.codexCatalogModels[0].contextWindow).toBe("");
  });
});
