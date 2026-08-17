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
}
