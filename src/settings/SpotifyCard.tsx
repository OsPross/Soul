import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAnim } from "../anim/AnimationProvider";
import { tr } from "../i18n";
import { useSpotifyStatus, type SpotifyStatus } from "../lib/spotify";
import { Button, Card, Row } from "./ui";

const REDIRECT = "http://127.0.0.1:43821/callback";

/** Settings → Wyspa: Spotify background control + optional Web API login (OAuth PKCE). */
export function SpotifyCard() {
  const lang = useAnim().cfg.uiLang;
  const [status, setStatus] = useSpotifyStatus();
  const [clientId, setClientId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (status?.clientId) setClientId((c) => c || status.clientId);
  }, [status?.clientId]);

  const run = async (cmd: "spotify_connect" | "spotify_disconnect") => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await invoke<SpotifyStatus>(cmd, cmd === "spotify_connect" ? { clientId: clientId.trim() } : {}));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const badge = status?.connected
    ? tr(lang, "✓ Połączono jako", "✓ Connected as") +
      ` ${status.displayName ?? "?"}${status.isPremium ? " (Premium)" : tr(lang, " (bez Premium)", " (no Premium)")}`
    : tr(lang, "Niepołączono", "Not connected");

  return (
    <Card
      title={tr(lang, "Spotify (sterowanie w tle)", "Spotify (background control)")}
      description={tr(
        lang,
        "Pauza, następny/poprzedni, przewijanie, losowo/powtarzanie i osobna głośność Spotify działają od razu — bez konfiguracji i bez wyciągania okna Spotify na wierzch.",
        "Play/pause, next/previous, seeking, shuffle/repeat and Spotify's own volume work right away — no setup, and the Spotify window never comes to the front.",
      )}
    >
      <Row
        stacked
        label={tr(lang, "Połącz ze Spotify (OAuth PKCE)", "Connect Spotify (OAuth PKCE)")}
        hint={tr(
          lang,
          `Włączanie dowolnego utworu/playlisty głosem, kolejka i polubienia w tle. 1. Otwórz developer.spotify.com/dashboard. 2. Utwórz aplikację z Redirect URI ${REDIRECT}. 3. Wklej Client ID i kliknij „Połącz ze Spotify”. Odtwarzanie przez API wymaga Premium.`,
          `Play any song/playlist by voice, queue and like tracks in the background. 1. Open developer.spotify.com/dashboard. 2. Create an app with Redirect URI ${REDIRECT}. 3. Paste the Client ID and click “Connect Spotify”. Playback over the API needs Premium.`,
        )}
      >
        <div className="flex gap-2">
          <input
            type="text"
            autoComplete="off"
            spellCheck={false}
            className="soul-input min-w-0 flex-1 px-3 py-2 text-[13px]"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="Spotify Client ID"
            disabled={busy || status?.connected}
          />
          {status?.connected ? (
            <Button variant="danger" onClick={() => void run("spotify_disconnect")} disabled={busy}>
              {tr(lang, "Rozłącz", "Disconnect")}
            </Button>
          ) : (
            <Button variant="primary" onClick={() => void run("spotify_connect")} disabled={busy || !clientId.trim()}>
              {busy ? tr(lang, "Czekam na przeglądarkę…", "Waiting for browser…") : tr(lang, "Połącz ze Spotify", "Connect Spotify")}
            </Button>
          )}
        </div>
      </Row>
      <p className={`mt-2 text-[12px] ${status?.connected ? "text-[var(--soul-success)]" : "text-[var(--soul-muted)]"}`}>
        {badge}
        {status && !status.installed && tr(lang, " · Spotify nie jest zainstalowany", " · Spotify is not installed")}
      </p>
      {error && <p className="mt-1 select-text break-all text-[12px] text-[var(--soul-danger)]">{error}</p>}
    </Card>
  );
}
