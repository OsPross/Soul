import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** `spotify::auth::SpotifyStatus` (no tokens ever reach the frontend). */
export interface SpotifyStatus {
  connected: boolean;
  clientId: string;
  displayName: string | null;
  isPremium: boolean;
  installed: boolean;
  running: boolean;
}

export interface SpotifyPlaylist {
  name: string;
  uri: string;
  image: string | null;
}

export interface SpotifyDevice {
  id: string;
  name: string;
  kind: string;
  isActive: boolean;
}

/** Account status, refreshed on `soul://spotify-status` (connect / disconnect in any window). */
export function useSpotifyStatus(): [SpotifyStatus | null, (s: SpotifyStatus) => void] {
  const [status, setStatus] = useState<SpotifyStatus | null>(null);
  useEffect(() => {
    invoke<SpotifyStatus>("spotify_auth_status").then(setStatus).catch(() => undefined);
    const un = listen<SpotifyStatus>("soul://spotify-status", (e) => setStatus(e.payload));
    return () => {
      void un.then((f) => f());
    };
  }, []);
  return [status, setStatus];
}

/** Same actions as the `spotify` voice tool (`spotify::run`). */
export function spotifyRun(args: Record<string, unknown>): Promise<string> {
  return invoke<string>("spotify_run", { args });
}
