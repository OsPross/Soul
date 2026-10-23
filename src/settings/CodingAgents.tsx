import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { tr, type UiLang } from "../i18n";
import { formatMinutes, summarize, weekStart, type History } from "../island/recap";
import { Button, Card, Row, Segmented, Toggle } from "./ui";

/** Mirrors `coding_agents::installer::CodingAgentStatus`. */
type AgentStatus = { id: string; name: string; configPath: string; detected: boolean; installed: boolean };
/** Mirrors `coding_agents::installer::HookPlanPreview`. */
type Preview = { diff: string; backup: boolean; configPath: string; fingerprint: string };

const DAYS: [string, string][] = [
  ["poniedziałek", "Monday"],
  ["wtorek", "Tuesday"],
  ["środa", "Wednesday"],
  ["czwartek", "Thursday"],
  ["piątek", "Friday"],
  ["sobota", "Saturday"],
  ["niedziela", "Sunday"],
];

/** Settings → Agent: coding agent hooks (diff preview, dated backup) and the weekly recap. */
export default function CodingAgentsCards({ lang }: { lang: UiLang }) {
  return (
    <>
      <HooksCard lang={lang} />
      <RecapCard lang={lang} />
    </>
  );
}

function HooksCard({ lang }: { lang: UiLang }) {
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [open, setOpen] = useState<{ id: string; install: boolean; preview: Preview } | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const refresh = useCallback(() => {
    invoke<AgentStatus[]>("coding_agents_status").then(setAgents).catch((e) => setMsg(String(e)));
  }, []);
  useEffect(refresh, [refresh]);

  const preview = (a: AgentStatus) => {
    setMsg(null);
    const install = !a.installed;
    invoke<Preview>("coding_agents_preview", { agent: a.id, install })
      .then((p) => setOpen({ id: a.id, install, preview: p }))
      .catch((e) => setMsg(String(e)));
  };
  const apply = () => {
    if (!open) return;
    invoke<string>("coding_agents_apply", { agent: open.id, install: open.install, fingerprint: open.preview.fingerprint })
      .then((m) => {
        setMsg(m);
        setOpen(null);
        refresh();
      })
      .catch((e) => setMsg(String(e)));
  };

  return (
    <Card
      title={tr(lang, "Integracje z agentami kodującymi", "Coding agents")}
      description={tr(
        lang,
        "Soul pokazuje na wyspie, co robi agent, prośby o zgodę („tak” / „nie” głosem) i diff edytowanych plików. Instalacja dopisuje hooki do konfiguracji agenta — Twoje inne hooki i statusLine zostają, a stary plik trafia do kopii z datą.",
        "Soul shows what the agent does on the Island, permission prompts (say “yes” / “no”) and a diff of edited files. Installing adds hooks to the agent's config — your other hooks and statusLine stay, and the old file is backed up with a date.",
      )}
    >
      {agents.map((a) => (
        <div key={a.id} className="space-y-2">
          <Row
            label={
              <span>
                {a.name}
                {a.installed && <span className="ml-2 text-[11px] text-[var(--soul-success,#34d399)]">● {tr(lang, "zainstalowane", "installed")}</span>}
                {!a.installed && !a.detected && <span className="ml-2 text-[11px] text-white/40">{tr(lang, "nie wykryto", "not detected")}</span>}
              </span>
            }
            hint={<span className="font-mono">{a.configPath}</span>}
          >
            <Button onClick={() => preview(a)}>
              {a.installed ? tr(lang, "Podgląd i odinstaluj", "Preview uninstall") : tr(lang, "Podgląd diffa i instaluj", "Preview & install")}
            </Button>
          </Row>
          {open?.id === a.id && (
            <div className="space-y-2 rounded-xl border border-white/10 bg-white/[0.03] p-3 text-[12px]">
              <pre className="soul-scroll max-h-56 overflow-auto rounded-lg bg-black/30 p-2 font-mono text-[11px] leading-snug text-white/80">
                {open.preview.diff || tr(lang, "(bez zmian)", "(no changes)")}
              </pre>
              {open.preview.backup && (
                <div className="text-white/50">
                  {tr(lang, "Przed zapisem powstanie kopia: ", "A backup is written first: ")}
                  <span className="font-mono">{open.preview.configPath}.bak-RRRRMMDD-GGMMSS</span>
                </div>
              )}
              <div className="flex justify-end gap-2">
                <Button onClick={() => setOpen(null)}>{tr(lang, "Anuluj", "Cancel")}</Button>
                <Button variant={open.install ? "primary" : "danger"} onClick={apply}>
                  {open.install ? tr(lang, "Zainstaluj", "Install") : tr(lang, "Odinstaluj", "Uninstall")}
                </Button>
              </div>
            </div>
          )}
        </div>
      ))}
      {msg && <div className="text-[12px] text-white/60">{msg}</div>}
    </Card>
  );
}

function RecapCard({ lang }: { lang: UiLang }) {
  const [history, setHistory] = useState<History | null>(null);
  const [week, setWeek] = useState<0 | 1>(0);
  const load = useCallback(() => {
    invoke<History | null>("coding_recap_history", { since: weekStart(Date.now(), 1) })
      .then(setHistory)
      .catch(() => undefined);
  }, []);
  useEffect(load, [load]);
  if (!history) return null;
  const s = summarize(history, weekStart(Date.now(), week));
  const stats: [string, string][] = [
    [tr(lang, "Czas kodowania", "Coding time"), formatMinutes(s.totalMinutes)],
    [tr(lang, "Sesje", "Sessions"), String(s.sessionCount)],
    [tr(lang, "Zmienione pliki", "Files changed"), String(s.filesChanged)],
    [tr(lang, "Linie", "Lines"), `+${s.linesAdded} −${s.linesRemoved}`],
    [tr(lang, "Polecenia", "Commands"), String(s.commandsRun)],
    [tr(lang, "Zgody", "Approvals"), `✓ ${s.permissionsAllowed} · ✕ ${s.permissionsDenied}`],
    [tr(lang, "Najczęstszy agent", "Top agent"), s.topAgent ?? "—"],
    [tr(lang, "Najczęstszy projekt", "Top project"), s.topProject ?? "—"],
    [tr(lang, "Najdłuższa sesja", "Longest session"), formatMinutes(s.longestSessionMinutes)],
    [tr(lang, "Najpracowitszy dzień", "Busiest day"), s.busiestDay === null ? "—" : tr(lang, ...DAYS[s.busiestDay])],
  ];
  return (
    <Card
      title={tr(lang, "Podsumowanie kodowania", "Coding recap")}
      description={tr(
        lang,
        "Liczone lokalnie z hooków agentów. Na dysk trafiają tylko liczby, nazwa agenta i nazwa folderu projektu — nigdy polecenia, prompty ani treść plików. Historia: 12 tygodni.",
        "Counted locally from the agents' hooks. Only counts, the agent name and the project folder name are stored — never commands, prompts or file contents. History: 12 weeks.",
      )}
      action={
        <Segmented
          size="sm"
          value={week}
          onChange={setWeek}
          options={[
            { value: 0, label: tr(lang, "Ten tydzień", "This week") },
            { value: 1, label: tr(lang, "Poprzedni", "Last week") },
          ]}
        />
      }
    >
      <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-[12.5px]">
        {stats.map(([k, v]) => (
          <div key={k} className="flex justify-between gap-3">
            <span className="text-[var(--soul-muted)]">{k}</span>
            <span className="truncate font-medium tabular-nums text-white/90">{v}</span>
          </div>
        ))}
      </div>
      <Row label={tr(lang, "Zbieraj historię", "Keep history")}>
        <Toggle
          checked={history.enabled}
          onChange={(v) => invoke("coding_recap_set_enabled", { enabled: v }).then(load).catch(() => undefined)}
        />
      </Row>
      <div className="flex justify-end">
        <Button variant="danger" onClick={() => invoke("coding_recap_clear").then(load).catch(() => undefined)}>
          {tr(lang, "Wyczyść historię", "Clear history")}
        </Button>
      </div>
    </Card>
  );
}
