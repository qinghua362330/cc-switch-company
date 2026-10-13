import { fireEvent, renderHook } from "@testing-library/react";
import { renderWithQueryClient as render } from "../utils/testQueryClient";
import { describe, expect, it, vi } from "vitest";
import { useForm } from "react-hook-form";
import { HermesFormFields } from "@/components/providers/forms/HermesFormFields";
import { useApiKeyLink } from "@/components/providers/forms/hooks/useApiKeyLink";
import { useProviderCategory } from "@/components/providers/forms/hooks/useProviderCategory";
import { hermesProviderPresets } from "@/config/hermesProviderPresets";
import { openclawProviderPresets } from "@/config/openclawProviderPresets";
import { Form } from "@/components/ui/form";

function OfficialHermesForm({ onChange }: { onChange: (key: string) => void }) {
  const form = useForm();
  return (
    <Form {...form}>
      <HermesFormFields
        baseUrl="https://inference-api.nousresearch.com/v1"
        onBaseUrlChange={vi.fn()}
        apiKey="old-key"
        onApiKeyChange={onChange}
        category="official"
        shouldShowApiKeyLink={false}
        websiteUrl=""
        apiMode="chat_completions"
        onApiModeChange={vi.fn()}
        models={[]}
        onModelsChange={vi.fn()}
        rateLimitDelay={0}
        onRateLimitDelayChange={vi.fn()}
      />
    </Form>
  );
}

describe("official provider form restoration", () => {
  it("lets Hermes official users rotate their API key", () => {
    const onChange = vi.fn();
    render(<OfficialHermesForm onChange={onChange} />);
    const input = document.querySelector<HTMLInputElement>(
      'input[type="password"]',
    )!;
    expect(input).toBeEnabled();
    fireEvent.change(input, { target: { value: "rotated-key" } });
    expect(onChange).toHaveBeenCalledWith("rotated-key");
  });

  it.each(["claude-desktop", "openclaw", "hermes"] as const)(
    "shows the API key link for %s third-party presets",
    (appId) => {
      const { result } = renderHook(() =>
        useApiKeyLink({
          appId,
          category: "third_party",
          selectedPresetId: null,
          presetEntries: [],
          formWebsiteUrl: "https://example.test/keys",
        }),
      );
      expect(result.current.shouldShowApiKeyLink).toBe(true);
      expect(result.current.websiteUrl).toBe("https://example.test/keys");
    },
  );

  it.each([
    ["hermes", hermesProviderPresets],
    ["openclaw", openclawProviderPresets],
  ] as const)(
    "derives %s category when selecting a preset",
    (appId, presets) => {
      const index = presets.findIndex((preset) => Boolean(preset.category));
      expect(index).toBeGreaterThanOrEqual(0);
      const { result, rerender } = renderHook(
        ({ selectedPresetId }) =>
          useProviderCategory({ appId, selectedPresetId, isEditMode: false }),
        { initialProps: { selectedPresetId: "custom" } },
      );
      expect(result.current.category).toBe("custom");
      rerender({ selectedPresetId: `${appId}-${index}` });
      expect(result.current.category).toBe(presets[index].category);
    },
  );
});
