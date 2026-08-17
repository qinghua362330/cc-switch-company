import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  show: vi.fn(),
  isMinimized: vi.fn(),
  unminimize: vi.fn(),
  setFocus: vi.fn(),
  message: vi.fn(),
  confirm: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    show: mocks.show,
    isMinimized: mocks.isMinimized,
    unminimize: mocks.unminimize,
    setFocus: mocks.setFocus,
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  message: mocks.message,
  confirm: mocks.confirm,
}));

import {
  confirmCompanySoftwareUpdate,
  showCompanyCatalogUpdatedDialog,
} from "./userAttention";

describe("company update attention", () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    mocks.show.mockReset().mockResolvedValue(undefined);
    mocks.isMinimized.mockReset().mockResolvedValue(false);
    mocks.unminimize.mockReset().mockResolvedValue(undefined);
    mocks.setFocus.mockReset().mockResolvedValue(undefined);
    mocks.message.mockReset().mockResolvedValue(undefined);
    mocks.confirm.mockReset().mockResolvedValue(false);
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it("shows, restores and focuses the app before the catalog dialog", async () => {
    mocks.isMinimized.mockResolvedValue(true);

    await showCompanyCatalogUpdatedDialog();

    expect(mocks.show).toHaveBeenCalledOnce();
    expect(mocks.isMinimized).toHaveBeenCalledOnce();
    expect(mocks.unminimize).toHaveBeenCalledOnce();
    expect(mocks.setFocus).toHaveBeenCalledOnce();
    expect(mocks.message).toHaveBeenCalledWith(
      "公司模型配置已更新，请重启 ChatGPT 获取最新配置信息。",
      expect.objectContaining({ title: "模型配置已更新" }),
    );
    expect(mocks.setFocus.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.message.mock.invocationCallOrder[0],
    );
  });

  it("does not unminimize an already visible app", async () => {
    await showCompanyCatalogUpdatedDialog();

    expect(mocks.unminimize).not.toHaveBeenCalled();
    expect(mocks.setFocus).toHaveBeenCalledOnce();
  });

  it("still shows the catalog dialog when foregrounding fails", async () => {
    mocks.show.mockRejectedValue(new Error("show failed"));
    mocks.setFocus.mockRejectedValue(new Error("focus failed"));

    await expect(showCompanyCatalogUpdatedDialog()).resolves.toBeUndefined();

    expect(mocks.isMinimized).toHaveBeenCalledOnce();
    expect(mocks.message).toHaveBeenCalledOnce();
    expect(warnSpy).toHaveBeenCalledTimes(2);
  });

  it.each([true, false])(
    "returns the native software update confirmation result: %s",
    async (confirmed) => {
      mocks.confirm.mockResolvedValue(confirmed);

      await expect(
        confirmCompanySoftwareUpdate("3.18.1", "候选版本"),
      ).resolves.toBe(confirmed);

      expect(mocks.setFocus.mock.invocationCallOrder[0]).toBeLessThan(
        mocks.confirm.mock.invocationCallOrder[0],
      );
      expect(mocks.confirm).toHaveBeenCalledWith(
        expect.stringContaining("v3.18.1"),
        expect.objectContaining({
          okLabel: "立即更新",
          cancelLabel: "稍后",
        }),
      );
    },
  );

  it("still asks for confirmation when foregrounding fails", async () => {
    mocks.isMinimized.mockRejectedValue(new Error("state unavailable"));
    mocks.confirm.mockResolvedValue(true);

    await expect(confirmCompanySoftwareUpdate("3.18.1")).resolves.toBe(true);

    expect(mocks.setFocus).toHaveBeenCalledOnce();
    expect(mocks.confirm).toHaveBeenCalledOnce();
    expect(warnSpy).toHaveBeenCalledOnce();
  });
});
