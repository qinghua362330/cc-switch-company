import { afterEach, describe, expect, it, vi } from "vitest";
import {
  readRememberedChoice,
  writeRememberedChoice,
} from "@/components/usage/rememberedChoice";

const KEY = "cc-switch.usage.test";
const METRICS = ["tokens", "requests", "cost"] as const;

describe("remembered usage choices", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.removeItem(KEY);
  });

  it("reads back what was written", () => {
    writeRememberedChoice(KEY, "cost");
    expect(readRememberedChoice(KEY, METRICS, "tokens")).toBe("cost");
  });

  it("falls back when nothing or an unknown value is stored", () => {
    expect(readRememberedChoice(KEY, METRICS, "tokens")).toBe("tokens");
    localStorage.setItem(KEY, "latency");
    expect(readRememberedChoice(KEY, METRICS, "tokens")).toBe("tokens");
  });

  it("keeps working when storage is unavailable", () => {
    const original = Object.getOwnPropertyDescriptor(window, "localStorage");
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: {
        getItem: () => {
          throw new Error("denied");
        },
        setItem: () => {
          throw new Error("denied");
        },
      },
    });
    try {
      expect(() => writeRememberedChoice(KEY, "cost")).not.toThrow();
      expect(readRememberedChoice(KEY, METRICS, "tokens")).toBe("tokens");
    } finally {
      if (original) Object.defineProperty(window, "localStorage", original);
      else Reflect.deleteProperty(window, "localStorage");
    }
  });
});
