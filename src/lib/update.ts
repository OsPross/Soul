import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";

/** Mirror of `updater::UpdateStatus` (Rust, `soul://update`). */
export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "ready"
  | "waiting"
  | "installing"
  | "installed"
  | "error";

export interface UpdateStatus {
  phase: UpdatePhase;
  currentVersion: string;
  version: string | null;
  notes: string | null;
  date: string | null;
  releaseUrl: string | null;
  /** `auto` = Soul installs it; `manual` = open the release page (.deb / .rpm / dev build). */
  install: "auto" | "manual";
  bundle: "nsis" | "msi" | "appimage" | "deb" | "rpm" | "dev";
  downloaded: number;
  total: number | null;
  error: string | null;
  lastCheck: number | null;
  autoUpdate: boolean;
  channel: "stable" | "beta";
}

/** Live update status (initial fetch + `soul://update`). */
export function useUpdate(): UpdateStatus | null {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | null = null;
    invoke<UpdateStatus>("update_status")
      .then((s) => alive && setStatus(s))
      .catch(() => undefined);
    listen<UpdateStatus>("soul://update", (e) => alive && setStatus(e.payload))
      .then((u) => (alive ? (unlisten = u) : u()))
      .catch(() => undefined);
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);
  return status;
}

/** An update the user can act on (notice on the Island, button in Settings). */
export function hasPendingUpdate(s: UpdateStatus | null): boolean {
  return Boolean(s?.version) && (s!.phase === "available" || s!.phase === "ready" || s!.phase === "installed");
}

/**
 * The one "install" action: package-manager installs open the release page; otherwise download
 * (if needed), wait for Soul to be idle, install, relaunch. On Windows the installer closes Soul
 * itself and starts the new version.
 */
export async function installUpdate(s: UpdateStatus | null): Promise<void> {
  if (!s) return;
  if (s.install === "manual") {
    await invoke("update_open_release");
    return;
  }
  await invoke("update_install");
  await relaunch();
}

export function formatBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} kB`;
  return `${n} B`;
}
