import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion } from "framer-motion";
import Markdown from "react-markdown";
import { useAnim } from "../anim/AnimationProvider";
import { tr, type UiLang } from "../i18n";
import { formatBytes, installUpdate, useUpdate, type UpdateStatus } from "../lib/update";
import { Button, Callout, Card, Row, Segmented, Toggle } from "./ui";

const RELEASES_URL = "https://github.com/OsPross/Soul/releases";

const BUNDLE_LABEL: Record<UpdateStatus["bundle"], string> = {
  nsis: "Windows (.exe)",
  msi: "Windows (.msi)",
  appimage: "Linux AppImage",
  deb: "Linux .deb",
  rpm: "Linux .rpm",
  dev: "dev build",
};

function statusLine(lang: UiLang, s: UpdateStatus): string {
  switch (s.phase) {
    case "checking":
      return tr(lang, "Sprawdzam…", "Checking…");
    case "upToDate":
      return tr(lang, "Masz najnowszą wersję.", "You're up to date.");
    case "available":
      return tr(lang, `Dostępna wersja v${s.version}.`, `Version v${s.version} is available.`);
    case "downloading":
      return tr(lang, `Pobieram v${s.version}…`, `Downloading v${s.version}…`);
    case "ready":
      return tr(lang, `v${s.version} pobrana — uruchom ponownie, aby zainstalować.`, `v${s.version} downloaded — restart to install.`);
    case "waiting":
      return tr(lang, "Czekam, aż Soul skończy mówić / nagrywać…", "Waiting for Soul to finish recording / speaking…");
    case "installing":
      return tr(lang, "Instaluję…", "Installing…");
    case "installed":
      return tr(lang, "Zainstalowano — uruchom ponownie.", "Installed — restart Soul.");
    case "error":
      return tr(lang, `Błąd aktualizacji: ${s.error ?? "?"}`, `Update error: ${s.error ?? "?"}`);
    default:
      return tr(lang, "Jeszcze nie sprawdzano.", "Not checked yet.");
  }
}

function formatDate(lang: UiLang, iso: string | null): string | null {
  if (!iso) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleDateString(lang === "en" ? "en-GB" : "pl-PL", { day: "numeric", month: "long", year: "numeric" });
}

export default function AboutPage({ lang }: { lang: UiLang }) {
  const anim = useAnim();
  const s = useUpdate();
  const [actionError, setActionError] = useState<string | null>(null);

  if (!s) {
    return <p className="text-[12px] text-[var(--soul-muted)]">{tr(lang, "Ładuję…", "Loading…")}</p>;
  }

  const run = (p: Promise<unknown>) => {
    setActionError(null);
    p.catch((e) => setActionError(String(e)));
  };
  const saveConfig = (autoUpdate: boolean, channel: UpdateStatus["channel"]) =>
    run(invoke("update_save_config", { autoUpdate, channel }));

  const hasUpdate = Boolean(s.version) && ["available", "downloading", "ready", "waiting", "installing", "installed"].includes(s.phase);
  const busy = s.phase === "checking" || s.phase === "downloading" || s.phase === "waiting" || s.phase === "installing";
  const pct = s.total ? Math.min(100, (s.downloaded / s.total) * 100) : null;
  const date = formatDate(lang, s.date);
  const pkg = s.bundle === "rpm" ? "rpm" : "deb";

  return (
    <>
      <Card title="Soul" description={tr(lang, "Asystent głosowy na pulpit.", "Voice AI desktop assistant.")}>
        <Row label={tr(lang, "Wersja", "Version")} hint={BUNDLE_LABEL[s.bundle]}>
          <span className="font-mono text-[13px] text-white/90">v{s.currentVersion}</span>
        </Row>
        <Row
          label={tr(lang, "Twoje dane", "Your data")}
          hint={tr(
            lang,
            "Ustawienia, pamięć i słowo-klucz są w folderze danych aplikacji — zostają po aktualizacji i ponownej instalacji.",
            "Settings, memory and the wake word live in the app data folder — they survive updates and reinstalls.",
          )}
        >
          <Button onClick={() => run(invoke("update_open_release"))}>{tr(lang, "Wydania na GitHub", "Releases on GitHub")}</Button>
        </Row>
      </Card>

      <Card
        title={tr(lang, "Aktualizacje", "Updates")}
        description={statusLine(lang, s)}
        action={
          <Button onClick={() => run(invoke("update_check"))} disabled={busy}>
            {s.phase === "checking" ? tr(lang, "Sprawdzam…", "Checking…") : tr(lang, "Sprawdź aktualizacje", "Check for updates")}
          </Button>
        }
      >
        <Row
          label={tr(lang, "Automatyczne aktualizacje", "Automatic updates")}
          hint={tr(
            lang,
            "Sprawdza przy starcie i co 6 godzin, pobiera w tle. Instalacja dopiero po kliknięciu — nigdy w trakcie nagrywania ani mówienia.",
            "Checks at startup and every 6 hours, downloads in the background. Installs only when you click — never while recording or speaking.",
          )}
        >
          <Toggle checked={s.autoUpdate} onChange={(v) => saveConfig(v, s.channel)} />
        </Row>
        <Row
          label={tr(lang, "Kanał", "Channel")}
          hint={tr(lang, "Beta dostaje też wersje testowe (prerelease).", "Beta also gets test versions (prereleases).")}
        >
          <Segmented
            size="sm"
            value={s.channel}
            options={[
              { value: "stable", label: tr(lang, "Stabilny", "Stable") },
              { value: "beta", label: "Beta" },
            ]}
            onChange={(v) => saveConfig(s.autoUpdate, v)}
          />
        </Row>

        {hasUpdate && (
          <div className="flex flex-col gap-3 rounded-2xl border border-white/10 bg-black/25 p-4">
            <div className="flex items-baseline justify-between gap-3">
              <div className="text-[14px] font-semibold">
                {tr(lang, "Nowa wersja", "New version")} <span className="font-mono">v{s.version}</span>
              </div>
              {date && <div className="text-[11.5px] text-[var(--soul-muted)]">{date}</div>}
            </div>

            {s.notes ? (
              <div className="ask-md soul-scroll max-h-56 overflow-y-auto text-[12.5px] leading-relaxed text-white/80">
                <Markdown components={{ a: ({ children }) => <span className="underline">{children}</span> }}>{s.notes}</Markdown>
              </div>
            ) : (
              <p className="text-[12px] text-[var(--soul-muted)]">{tr(lang, "Brak opisu zmian.", "No release notes.")}</p>
            )}

            {(s.phase === "downloading" || s.phase === "ready" || s.phase === "waiting" || s.phase === "installing") && (
              <div>
                <div className="h-1.5 overflow-hidden rounded-full bg-white/10">
                  <motion.div
                    className="h-full rounded-full bg-[var(--soul-accent)]"
                    initial={false}
                    animate={{ width: `${s.phase === "downloading" ? (pct ?? 8) : 100}%` }}
                    transition={{ duration: anim.dur(200) }}
                  />
                </div>
                <div className="mt-1 text-[11px] tabular-nums text-[var(--soul-muted)]">
                  {formatBytes(s.downloaded)}
                  {s.total ? ` / ${formatBytes(s.total)}` : ""}
                  {pct !== null && s.phase === "downloading" ? ` · ${Math.round(pct)}%` : ""}
                </div>
              </div>
            )}

            <div className="flex flex-wrap items-center justify-end gap-2">
              {s.install === "manual" ? (
                <Button variant="primary" onClick={() => run(invoke("update_open_release"))}>
                  {tr(lang, "Pobierz nowy pakiet", "Download new package")}
                </Button>
              ) : s.phase === "available" ? (
                <>
                  <Button onClick={() => run(invoke("update_download"))}>{tr(lang, "Tylko pobierz", "Download only")}</Button>
                  <Button variant="primary" onClick={() => run(installUpdate(s))}>
                    {tr(lang, "Pobierz i zainstaluj", "Download & install")}
                  </Button>
                </>
              ) : (
                <Button variant="primary" disabled={busy} onClick={() => run(installUpdate(s))}>
                  {s.phase === "waiting" || s.phase === "installing"
                    ? tr(lang, "Instaluję…", "Installing…")
                    : tr(lang, "Uruchom ponownie", "Restart")}
                </Button>
              )}
            </div>
          </div>
        )}

        {s.install === "manual" && s.bundle !== "dev" && (
          <Callout>
            {tr(
              lang,
              `Soul zainstalowany z pakietu .${pkg} aktualizuje się przez nowy pakiet: pobierz go i zainstaluj (`,
              `Soul installed from a .${pkg} package updates via the new package: download it and install it (`,
            )}
            <code className="font-mono">
              {pkg === "rpm" ? "sudo dnf install ./Soul-*.rpm" : "sudo apt install ./Soul_*.deb"}
            </code>
            {tr(lang, "). Twoje dane zostają. Automatyczne aktualizacje działają w wersji AppImage.", "). Your data is kept. Automatic updates work in the AppImage build.")}
          </Callout>
        )}
        {s.bundle === "dev" && (
          <Callout>
            {tr(
              lang,
              "To wersja deweloperska — sprawdzanie działa, ale instalacja jest dostępna tylko w zainstalowanej aplikacji.",
              "This is a dev build — checking works, but installing is only available in an installed app.",
            )}
          </Callout>
        )}
        {actionError && <Callout tone="danger">{actionError}</Callout>}
      </Card>

      <p className="px-1 text-[11px] text-white/30">
        <a className="hover:text-white/60" href={RELEASES_URL} onClick={(e) => (e.preventDefault(), run(invoke("update_open_release")))}>
          github.com/OsPross/Soul
        </a>
      </p>
    </>
  );
}
