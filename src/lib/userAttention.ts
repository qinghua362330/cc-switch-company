import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, message } from "@tauri-apps/plugin-dialog";

async function runForegroundOperation<T>(
  operation: string,
  callback: () => T | Promise<T>,
): Promise<T | undefined> {
  try {
    return await callback();
  } catch (error) {
    console.warn(`[UserAttention] Failed to ${operation}`, error);
    return undefined;
  }
}

export async function bringMainWindowToFront(): Promise<void> {
  const appWindow = await runForegroundOperation(
    "get the current window",
    getCurrentWindow,
  );
  if (!appWindow) return;

  await runForegroundOperation("show the main window", () => appWindow.show());
  const isMinimized = await runForegroundOperation(
    "read the main window state",
    () => appWindow.isMinimized(),
  );
  if (isMinimized) {
    await runForegroundOperation("restore the main window", () =>
      appWindow.unminimize(),
    );
  }
  await runForegroundOperation("focus the main window", () =>
    appWindow.setFocus(),
  );
}

export async function showCompanyCatalogUpdatedDialog(): Promise<void> {
  await bringMainWindowToFront();
  await message("公司模型配置已更新，请重启 ChatGPT 获取最新配置信息。", {
    title: "模型配置已更新",
    kind: "info",
    buttons: { ok: "知道了" },
  });
}

export async function confirmCompanySoftwareUpdate(
  version: string,
  notes?: string,
): Promise<boolean> {
  await bringMainWindowToFront();

  const detail = notes?.trim()
    ? `\n\n${notes.trim()}`
    : "\n\n确认后将打开终端执行公司版一键更新。";

  return await confirm(`检测到 CC Switch 新版本 v${version}。${detail}`, {
    title: "发现 CC Switch 新版本",
    kind: "info",
    okLabel: "立即更新",
    cancelLabel: "稍后",
  });
}
