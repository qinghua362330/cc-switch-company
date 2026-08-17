import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getVersion: vi.fn(),
  invoke: vi.fn(),
  check: vi.fn(),
}));

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: mocks.getVersion,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: mocks.check,
}));

import { checkForUpdate } from "./updater";

describe("checkForUpdate", () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    mocks.getVersion.mockReset();
    mocks.invoke.mockReset();
    mocks.check.mockReset();
    mocks.getVersion.mockResolvedValue("3.16.4");
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it("优先使用公司远程清单提示新版本", async () => {
    mocks.invoke.mockResolvedValue({
      version: "3.16.5",
      notes: "修复安装包",
      pub_date: "2026-06-29",
      installer_url: "https://leharrt.com/cc-switch/install.sh",
      download_url: "https://leharrt.com/cc-switch/CC Switch.app.tar.gz",
      release_notes_url: "https://leharrt.com/cc-switch/",
    });

    await expect(checkForUpdate()).resolves.toEqual({
      status: "available",
      info: {
        currentVersion: "3.16.4",
        availableVersion: "3.16.5",
        notes: "修复安装包",
        pubDate: "2026-06-29",
        installerUrl: "https://leharrt.com/cc-switch/install.sh",
        downloadUrl: "https://leharrt.com/cc-switch/CC Switch.app.tar.gz",
        releaseNotesUrl: "https://leharrt.com/cc-switch/",
        source: "company",
      },
    });
    expect(mocks.check).not.toHaveBeenCalled();
  });

  it("公司远程清单版本不高于当前版本时不提示更新", async () => {
    mocks.invoke.mockResolvedValue({
      version: "3.16.4",
      installer_url: "https://leharrt.com/cc-switch/install.sh",
    });

    await expect(checkForUpdate()).resolves.toEqual({ status: "up-to-date" });
    expect(mocks.check).not.toHaveBeenCalled();
  });

  it("公司远程清单不可用时直接暴露错误，不切换到公共更新渠道", async () => {
    mocks.invoke.mockRejectedValue(new Error("manifest unavailable"));

    await expect(checkForUpdate({ timeout: 1234 })).rejects.toThrow(
      "manifest unavailable",
    );
    expect(mocks.check).not.toHaveBeenCalled();
  });
});
