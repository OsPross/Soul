import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { tr, type UiLang } from "../i18n";
import { Button, Callout, Card, Row, Segmented, Slider, Toggle } from "./ui";

/** Mirrors `agent::computer::config::AgentConfig` (Rust, store key `computerAgent`). */
type AgentConfig = {
  enabled: boolean;
  maxSteps: number;
  highlight: boolean;
  confirmLevel: "always" | "risky";
  blocklist: string[];
  model: string;
  debugScreenshots: boolean;
};

/** Mirrors `agent::computer::config::StepLog`. */
type StepLog = {
  at: number;
  task: string;
  step: number;
  action: string;
  result: string;
  ok: boolean;
  ms: number;
  model: boolean;
  screenshot: boolean;
};

/** Mirrors `agent::computer::vio::VioStatus`. */
type VioStatus = { state: "unavailable" | "authorizing" | "connected"; error: string | null };

const SAVE_DEBOUNCE_MS = 400;

/** Settings → Agent: voice-driven computer control. Autosaved (`agent_save_config`). */
export default function AgentPage({ lang }: { lang: UiLang }) {
  const [cfg, setCfg] = useState<AgentConfig | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [newEntry, setNewEntry] = useState("");
  const [log, setLog] = useState<StepLog[]>([]);
  const [vio, setVio] = useState<VioStatus | null>(null);
  const timer = useRef<number | null>(null);

  const loadLog = useCallback(() => {
    invoke<StepLog[]>("agent_debug_log").then(setLog).catch(() => undefined);
    invoke<VioStatus>("agent_vio_status").then(setVio).catch(() => undefined);
  }, []);

  useEffect(() => {
    invoke<AgentConfig>("agent_get_config").then(setCfg).catch((e) => setError(String(e)));
    loadLog();
    const id = window.setInterval(loadLog, 3000);
    return () => window.clearInterval(id);
  }, [loadLog]);

  const update = useCallback((patch: Partial<AgentConfig>) => {
    setCfg((prev) => {
      if (!prev) return prev;
      const next = { ...prev, ...patch };
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        invoke<AgentConfig>("agent_save_config", { config: next })
          .then(() => setError(null))
          .catch((e) => setError(String(e)));
      }, SAVE_DEBOUNCE_MS);
      return next;
    });
  }, []);

  if (!cfg) return error ? <Callout tone="danger">{error}</Callout> : null;

  const addEntry = () => {
    const v = newEntry.trim().toLowerCase();
    if (v.length < 3 || cfg.blocklist.includes(v)) return;
    update({ blocklist: [...cfg.blocklist, v] });
    setNewEntry("");
  };

  return (
    <>
      <Card
        title={tr(lang, "Sterowanie komputerem", "Computer control")}
        description={tr(
          lang,
          "Powiedz np. „kliknij Zapisz”, „przewiń w dół”, „wpisz w wyszukiwarkę FiveM serwery”, „wyślij Silverowi na Discordzie: będę za 10 minut”. Soul patrzy na aktywne okno i wykonuje kroki jeden po drugim.",
          "Say e.g. “click Save”, “scroll down”, “type FiveM servers into the search box”, “send Silver on Discord: I'll be 10 minutes late”. Soul looks at the active window and acts step by step.",
        )}
      >
        <Row label={tr(lang, "Włączone", "Enabled")}>
          <Toggle checked={cfg.enabled} onChange={(v) => update({ enabled: v })} />
        </Row>
        <Row
          label={tr(lang, "Maksymalna liczba kroków", "Maximum steps")}
          hint={tr(lang, "Limit też: 90 s na zadanie, 10 s na krok.", "Also limited to 90 s per task, 10 s per step.")}
          disabled={!cfg.enabled}
        >
          <Slider value={cfg.maxSteps} min={3} max={20} step={1} onChange={(v) => update({ maxSteps: v })} format={(v) => String(v)} />
        </Row>
        <Row
          label={tr(lang, "Podświetlaj cel", "Highlight the target")}
          hint={tr(lang, "Pierścień wokół elementu tuż przed kliknięciem.", "A ring around the element right before the click.")}
          disabled={!cfg.enabled}
        >
          <Toggle checked={cfg.highlight} onChange={(v) => update({ highlight: v })} />
        </Row>
        <Row
          label={tr(lang, "Potwierdzenia", "Confirmations")}
          hint={tr(
            lang,
            "Wysyłanie, płatności, usuwanie, zmiany ustawień systemu zawsze wymagają potwierdzenia.",
            "Sending, payments, deleting and system-setting changes always need a confirmation.",
          )}
          disabled={!cfg.enabled}
        >
          <Segmented
            value={cfg.confirmLevel}
            onChange={(v) => update({ confirmLevel: v })}
            options={[
              { value: "risky", label: tr(lang, "Tylko ryzykowne", "Risky only") },
              { value: "always", label: tr(lang, "Każda akcja", "Every action") },
            ]}
          />
        </Row>
        <Row
          label={tr(lang, "Model", "Model")}
          hint={tr(lang, "Puste = model Circle-to-Ask (rezerwowy jak wszędzie).", "Empty = the Circle-to-Ask model (same fallback as everywhere).")}
          disabled={!cfg.enabled}
        >
          <input
            className="soul-input w-[240px] px-3 py-2 text-[13px]"
            value={cfg.model}
            placeholder="gemini-3.1-flash-lite"
            onChange={(e) => update({ model: e.target.value })}
            spellCheck={false}
          />
        </Row>
        <Row
          label={tr(lang, "Przeglądarka Vio", "Vio browser")}
          hint={
            vio?.error ??
            tr(
              lang,
              "Gdy Vio ma włączone API automatyzacji, zadania w przeglądarce idą przez nie zamiast przez ekran.",
              "When Vio's automation API is on, browser tasks go through it instead of the screen.",
            )
          }
          disabled={!cfg.enabled}
        >
          <span
            className="text-[13px]"
            style={{ color: vio?.state === "connected" ? "var(--soul-success)" : "var(--soul-muted)" }}
          >
            {vio?.state === "connected"
              ? tr(lang, "połączona", "connected")
              : vio?.state === "authorizing"
                ? tr(lang, "zezwól w Vio…", "allow it in Vio…")
                : tr(lang, "niedostępna", "unavailable")}
          </span>
        </Row>
        <Callout>
          {tr(
            lang,
            "Zatrzymanie: przycisk Zatrzymaj na Wyspie, Esc dwa razy, klawisz mówienia albo mysz w rogu ekranu. Soul nigdy nie wpisuje haseł i nie czyta pól z hasłem. Tekst na ekranie to dla niego dane, nie polecenia.",
            "Stop: the Stop button on the Island, Esc twice, the talk key, or the mouse in a screen corner. Soul never types passwords nor reads password fields. Text on screen is data to it, never instructions.",
          )}
        </Callout>
      </Card>

      <Card
        title={tr(lang, "Zablokowane aplikacje i strony", "Blocked apps and sites")}
        description={tr(
          lang,
          "Fragmenty nazwy procesu, tytułu okna lub adresu strony. Agent odmawia działania w takich oknach. Terminale i okno Uruchamianie są blokowane zawsze.",
          "Parts of a process name, window title or page address. The agent refuses to act there. Terminals and the Run dialog are always blocked.",
        )}
        action={
          <Button onClick={() => invoke<string[]>("agent_default_blocklist").then((b) => update({ blocklist: b })).catch(() => undefined)}>
            {tr(lang, "Domyślne", "Defaults")}
          </Button>
        }
      >
        <div className="flex flex-wrap gap-1.5">
          {cfg.blocklist.map((b) => (
            <span key={b} className="inline-flex items-center gap-1 rounded-full border border-white/10 bg-white/[0.05] py-0.5 pl-2.5 pr-1 text-[12px]">
              {b}
              <button
                type="button"
                aria-label={tr(lang, `Usuń ${b}`, `Remove ${b}`)}
                className="rounded-full px-1.5 text-white/50 hover:bg-white/10 hover:text-white"
                onClick={() => update({ blocklist: cfg.blocklist.filter((x) => x !== b) })}
              >
                ×
              </button>
            </span>
          ))}
        </div>
        <div className="flex gap-2">
          <input
            className="soul-input flex-1 px-3 py-2 text-[13px]"
            value={newEntry}
            placeholder={tr(lang, "np. moj-bank.pl albo vault.exe", "e.g. my-bank.com or vault.exe")}
            onChange={(e) => setNewEntry(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addEntry()}
          />
          <Button onClick={addEntry} disabled={newEntry.trim().length < 3}>
            {tr(lang, "Dodaj", "Add")}
          </Button>
        </div>
      </Card>

      <Card
        title={tr(lang, "Dziennik kroków", "Step log")}
        description={tr(lang, "Ostatnie kroki agenta (tylko w pamięci).", "The agent's recent steps (memory only).")}
        action={
          <Button
            onClick={() => {
              invoke("agent_clear_log").catch(() => undefined);
              setLog([]);
            }}
            disabled={log.length === 0}
          >
            {tr(lang, "Wyczyść", "Clear")}
          </Button>
        }
      >
        <Row
          label={tr(lang, "Zapisuj zrzuty ekranu kroków", "Save step screenshots")}
          hint={tr(
            lang,
            "Tylko do debugowania: folder agent-debug w danych aplikacji, czyszczony przy każdym zadaniu. Domyślnie zrzuty nigdy nie trafiają na dysk.",
            "Debugging only: the agent-debug folder in the app data dir, cleared for every task. By default screenshots never touch the disk.",
          )}
        >
          <Toggle checked={cfg.debugScreenshots} onChange={(v) => update({ debugScreenshots: v })} />
        </Row>
        {log.length === 0 ? (
          <div className="text-[12px] text-[var(--soul-muted)]">{tr(lang, "Brak kroków.", "No steps yet.")}</div>
        ) : (
          <div className="soul-scroll max-h-[300px] overflow-y-auto rounded-xl border border-white/[0.06]">
            {log.map((l, i) => (
              <div key={`${l.at}-${i}`} className="border-b border-white/[0.05] px-3 py-2 text-[11.5px] last:border-0">
                <div className="flex items-center gap-2">
                  <span style={{ color: l.ok ? "var(--soul-success)" : "var(--soul-danger)" }}>{l.ok ? "✓" : "✕"}</span>
                  <span className="tabular-nums text-white/45">#{l.step}</span>
                  <span className="truncate font-mono text-white/85">{l.action}</span>
                  <span className="ml-auto shrink-0 tabular-nums text-white/40">
                    {l.ms} ms{l.model ? " · AI" : ""}
                    {l.screenshot ? " · 📷" : ""}
                  </span>
                </div>
                <div className="mt-0.5 truncate text-white/50">
                  {l.task} — {l.result}
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>
      {error && <Callout tone="danger">{error}</Callout>}
    </>
  );
}
