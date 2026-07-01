import { deepClone } from "@/utils/deepClone";

const createDefaultCompanyAuthState = (): unknown => ({
  authenticated: true,
  user: {
    display_name: "测试员工",
    email: "tester@example.com",
  },
  base_url: "https://leharrt.com",
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
});

let companyAuthState: unknown = createDefaultCompanyAuthState();

export const resetCompanyAuthState = () => {
  companyAuthState = createDefaultCompanyAuthState();
};

export const getCompanyAuthState = () => deepClone(companyAuthState);

export const setCompanyAuthState = (state: unknown) => {
  companyAuthState = deepClone(state);
};
