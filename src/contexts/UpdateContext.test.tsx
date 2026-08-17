import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  checkForUpdate: vi.fn(),
  confirmCompanySoftwareUpdate: vi.fn(),
  launchInstaller: vi.fn(),
  toastError: vi.fn(),
}));

vi.mock("../lib/updater", () => ({
  checkForUpdate: mocks.checkForUpdate,
}));

vi.mock("@/lib/userAttention", () => ({
  confirmCompanySoftwareUpdate: mocks.confirmCompanySoftwareUpdate,
}));

vi.mock("@/lib/api", () => ({
  settingsApi: {
    launchCcSwitchUpdateInstaller: mocks.launchInstaller,
  },
}));

vi.mock("sonner", () => ({
  toast: { error: mocks.toastError },
}));

import { UpdateProvider } from "./UpdateContext";

const updateInfo = {
  currentVersion: "3.18.0",
  availableVersion: "3.18.1",
  notes: "灰度候选版本",
  installerUrl:
    "https://github.com/qinghua362330/cc-switch-company/releases/download/v3.18.1-company.1/install-v3.18.1-company.1.sh",
  source: "company" as const,
};

async function renderAndRunStartupCheck(): Promise<void> {
  render(
    <UpdateProvider>
      <div>ready</div>
    </UpdateProvider>,
  );

  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
    await Promise.resolve();
  });
}

describe("UpdateProvider company update prompt", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    localStorage.clear();
    mocks.checkForUpdate.mockReset().mockResolvedValue({
      status: "available",
      info: updateInfo,
    });
    mocks.confirmCompanySoftwareUpdate.mockReset().mockResolvedValue(false);
    mocks.launchInstaller.mockReset().mockResolvedValue(true);
    mocks.toastError.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("launches the pinned installer only after confirmation", async () => {
    mocks.confirmCompanySoftwareUpdate.mockResolvedValue(true);

    await renderAndRunStartupCheck();

    expect(mocks.launchInstaller).toHaveBeenCalledWith(updateInfo.installerUrl);
  });

  it("does not launch the installer when confirmation is declined", async () => {
    await renderAndRunStartupCheck();

    expect(mocks.confirmCompanySoftwareUpdate).toHaveBeenCalledOnce();
    expect(mocks.launchInstaller).not.toHaveBeenCalled();
  });

  it("does not focus or prompt for a dismissed version", async () => {
    localStorage.setItem(
      "ccswitch:update:dismissedVersion",
      updateInfo.availableVersion,
    );

    await renderAndRunStartupCheck();

    expect(mocks.checkForUpdate).toHaveBeenCalledOnce();
    expect(mocks.confirmCompanySoftwareUpdate).not.toHaveBeenCalled();
    expect(mocks.launchInstaller).not.toHaveBeenCalled();
  });
});
