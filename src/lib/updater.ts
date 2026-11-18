import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { isUpdateAvailable } from "@/lib/version";

export type UpdateChannel = "stable" | "beta";

export interface UpdateInfo {
  currentVersion: string;
  availableVersion: string;
  notes?: string;
  pubDate?: string;
  installerUrl?: string;
  downloadUrl?: string;
  releaseNotesUrl?: string;
  source?: "company" | "tauri";
}

/** 自动更新在后台下好、等用户确认安装的版本（后端 `app_update::ReadyUpdate`）。 */
export interface ReadyUpdate {
  version: string;
  notes?: string | null;
}

/** 后端 `app_update::AutoUpdateStatus`。 */
export interface AutoUpdateStatus {
  /** 这台机器能不能自动安装（便携版、Linux 非 AppImage 不行） */
  supported: boolean;
  /** 自动下载开关是否打开 */
  enabled: boolean;
  downloading: boolean;
  ready: ReadyUpdate | null;
  /** 有应用在用路由服务：重启会切断进行中的请求 */
  routingInUse: boolean;
}

/** 待安装版本变了的事件，载荷是 {@link ReadyUpdate}；null 表示下好的版本被撤回 */
export const UPDATE_READY_EVENT = "app-update-ready";

export interface CheckOptions {
  timeout?: number;
  channel?: UpdateChannel;
  manifestUrl?: string;
}

interface RemoteUpdateManifest {
  version: string;
  notes?: string | null;
  pub_date?: string | null;
  installer_url?: string | null;
  download_url?: string | null;
  release_notes_url?: string | null;
}

export async function getCurrentVersion(): Promise<string> {
  try {
    return await getVersion();
  } catch {
    return "";
  }
}

function isCompanyUpdateAvailable(current: string, latest: string): boolean {
  // Older company builds reported only the upstream version (iteration zero).
  // Company iterations are patches on that base, not a SemVer beta channel.
  const pattern = /^(\d+\.\d+\.\d+)(?:-company\.([1-9]\d*))?$/;
  const installed = current.match(pattern);
  const offered = latest.match(pattern);
  if (installed && offered && installed[1] === offered[1]) {
    const currentIteration = BigInt(installed[2] ?? "0");
    const latestIteration = BigInt(offered[2] ?? "0");
    return latestIteration > currentIteration;
  }
  return isUpdateAvailable(current, latest);
}

export async function checkForUpdate(
  opts: CheckOptions = {},
): Promise<
  { status: "up-to-date" } | { status: "available"; info: UpdateInfo }
> {
  const currentVersion = await getCurrentVersion();

  const manifest = await invoke<RemoteUpdateManifest>(
    "check_cc_switch_update_manifest",
    { manifestUrl: opts.manifestUrl },
  );
  if (!isCompanyUpdateAvailable(currentVersion, manifest.version)) {
    return { status: "up-to-date" };
  }

  return {
    status: "available",
    info: {
      currentVersion,
      availableVersion: manifest.version,
      notes: manifest.notes ?? undefined,
      pubDate: manifest.pub_date ?? undefined,
      installerUrl: manifest.installer_url ?? undefined,
      downloadUrl: manifest.download_url ?? undefined,
      releaseNotesUrl: manifest.release_notes_url ?? undefined,
      source: "company",
    },
  };
}
