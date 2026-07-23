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

export async function checkForUpdate(
  opts: CheckOptions = {},
): Promise<
  { status: "up-to-date" } | { status: "available"; info: UpdateInfo }
> {
  const currentVersion = await getCurrentVersion();

  try {
    const manifest = await invoke<RemoteUpdateManifest>(
      "check_cc_switch_update_manifest",
      { manifestUrl: opts.manifestUrl },
    );
    if (!isUpdateAvailable(currentVersion, manifest.version)) {
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
  } catch (error) {
    console.warn("[updater] Company update manifest check failed", error);
  }

  // 动态引入，避免在未安装插件时导致打包期问题。公司更新清单不可用时，
  // 保留原 Tauri updater 作为兜底。
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check({ timeout: opts.timeout ?? 30000 } as any);

  if (!update) {
    return { status: "up-to-date" };
  }

  const info: UpdateInfo = {
    currentVersion,
    availableVersion: (update as any).version ?? "",
    notes: (update as any).notes,
    pubDate: (update as any).date,
    source: "tauri",
  };

  return { status: "available", info };
}
