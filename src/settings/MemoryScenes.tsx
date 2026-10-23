import { useCallback, useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";
import { tr } from "../i18n";
import { MEMORY_CATEGORIES, STEP_TYPES, type MemoryFact, type Scene, type SceneStep, type SceneStepType } from "./model";
import type { SettingsCtx } from "./Settings";
import { Button, Callout, Card, Row, Select, Toggle } from "./ui";

type P = { s: SettingsCtx };

function catLabel(id: string, lang: string) {
  const c = MEMORY_CATEGORIES.find((x) => x.id === id);
  return c ? (lang === "en" ? c.en : c.pl) : id;
}

function when(ms: number, lang: string) {
  if (!ms) return "";
  return new Date(ms).toLocaleString(lang === "en" ? "en-GB" : "pl-PL", {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// ───────────────────────────── Pamięć ─────────────────────────────
export function MemoryPage({ s }: P) {
  const lang = useAnim().cfg.uiLang;
  const { form, patch } = s;
  const [facts, setFacts] = useState<MemoryFact[]>([]);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("");
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState({ text: "", category: "inne" });
  const [newFact, setNewFact] = useState({ text: "", category: "inne" });
  const [confirmClear, setConfirmClear] = useState(false);

  const reload = useCallback(async () => {
    try {
      setFacts(await invoke<MemoryFact[]>("memory_list", { query: query.trim() || null }));
    } catch (e) {
      s.setError(String(e));
    }
  }, [query, s]);

  useEffect(() => {
    const id = window.setTimeout(() => void reload(), 180);
    return () => window.clearTimeout(id);
  }, [reload]);

  useEffect(() => {
    let alive = true;
    const unsub = listen("soul://memory-changed", () => void reload()).then((u) => (alive ? u : (u(), undefined)));
    return () => {
      alive = false;
      void unsub.then((u) => u?.());
    };
  }, [reload]);

  const shown = useMemo(() => (category ? facts.filter((f) => f.category === category) : facts), [facts, category]);

  const run = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
      await reload();
    } catch (e) {
      s.setError(String(e));
    }
  };

  return (
    <>
      <Card
        title={tr(lang, "Pamięć Soul", "Soul's memory")}
        description={tr(
          lang,
          "Fakty zapisywane lokalnie (SQLite na tym komputerze). Do poleceń trafia maks. 15 najtrafniejszych. Hasła i klucze nigdy nie są zapamiętywane.",
          "Facts stored locally (SQLite on this computer). At most the 15 most relevant go into prompts. Passwords and keys are never stored.",
        )}
      >
        <Row
          label={tr(lang, "Zapamiętuj automatycznie", "Remember automatically")}
          hint={tr(
            lang,
            "Gdy w poleceniu powiesz np. „gram na serwerze X” albo „moja playlista to Y”, wyspa pokaże „Zapamiętałem: …” z opcją Cofnij.",
            "When a command says e.g. “I play on server X”, the Island shows “Remembered: …” with Undo.",
          )}
        >
          <Toggle checked={form.autoRemember ?? true} onChange={(autoRemember) => patch({ autoRemember })} />
        </Row>
        <Callout>
          {tr(lang, "Głosem: „zapamiętaj, że…”, „zapomnij o…”, „co pamiętasz o…”, „zanotuj…”.", "By voice: “remember that…”, “forget…”, “what do you remember about…”, “note…”.")}
        </Callout>
      </Card>

      <Card title={tr(lang, "Dodaj", "Add")}>
        <div className="flex items-center gap-2">
          <input
            value={newFact.text}
            onChange={(e) => setNewFact({ ...newFact, text: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter" && newFact.text.trim())
                void run(async () => {
                  await invoke("memory_add", newFact);
                  setNewFact({ text: "", category: newFact.category });
                });
            }}
            placeholder={tr(lang, "np. Gram na serwerze CityRP", "e.g. I play on the CityRP server")}
            className="soul-input min-w-0 flex-1 px-3 py-2 text-[13px]"
          />
          <Select value={newFact.category} onChange={(category) => setNewFact({ ...newFact, category })} className="w-[140px]">
            {MEMORY_CATEGORIES.map((c) => (
              <option key={c.id} value={c.id}>
                {lang === "en" ? c.en : c.pl}
              </option>
            ))}
          </Select>
          <Button
            variant="primary"
            disabled={!newFact.text.trim()}
            onClick={() =>
              void run(async () => {
                await invoke("memory_add", newFact);
                setNewFact({ text: "", category: newFact.category });
              })
            }
          >
            {tr(lang, "Dodaj", "Add")}
          </Button>
        </div>
      </Card>

      <Card
        title={`${tr(lang, "Wspomnienia", "Memories")} · ${shown.length}`}
        action={
          confirmClear ? (
            <div className="flex gap-1.5">
              <Button onClick={() => setConfirmClear(false)}>{tr(lang, "Anuluj", "Cancel")}</Button>
              <Button
                variant="danger"
                onClick={() =>
                  void run(async () => {
                    await invoke("memory_clear");
                    setConfirmClear(false);
                  })
                }
              >
                {tr(lang, "Usuń wszystko", "Delete all")}
              </Button>
            </div>
          ) : (
            <Button variant="danger" disabled={facts.length === 0} onClick={() => setConfirmClear(true)}>
              {tr(lang, "Wyczyść", "Clear all")}
            </Button>
          )
        }
      >
        <div className="flex items-center gap-2">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={tr(lang, "Szukaj…", "Search…")}
            className="soul-input min-w-0 flex-1 px-3 py-2 text-[13px]"
          />
          <Select value={category} onChange={setCategory} className="w-[150px]">
            <option value="">{tr(lang, "Wszystkie", "All")}</option>
            {MEMORY_CATEGORIES.map((c) => (
              <option key={c.id} value={c.id}>
                {lang === "en" ? c.en : c.pl}
              </option>
            ))}
          </Select>
        </div>
        {shown.length === 0 ? (
          <p className="py-6 text-center text-[12px] text-[var(--soul-muted)]">
            {tr(lang, "Nic tu jeszcze nie ma.", "Nothing here yet.")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            <AnimatePresence initial={false}>
              {shown.map((f) => (
                <motion.li
                  key={f.id}
                  layout
                  initial={{ opacity: 0, y: 4 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, x: -12 }}
                  className="rounded-2xl border border-white/[0.06] bg-white/[0.03] px-3.5 py-2.5"
                >
                  {editing === f.id ? (
                    <div className="flex items-center gap-2">
                      <input
                        autoFocus
                        value={draft.text}
                        onChange={(e) => setDraft({ ...draft, text: e.target.value })}
                        className="soul-input min-w-0 flex-1 px-3 py-1.5 text-[13px]"
                      />
                      <Select value={draft.category} onChange={(c) => setDraft({ ...draft, category: c })} className="w-[130px]">
                        {MEMORY_CATEGORIES.map((c) => (
                          <option key={c.id} value={c.id}>
                            {lang === "en" ? c.en : c.pl}
                          </option>
                        ))}
                      </Select>
                      <Button onClick={() => setEditing(null)}>✕</Button>
                      <Button
                        variant="primary"
                        onClick={() =>
                          void run(async () => {
                            await invoke("memory_update", { id: f.id, ...draft });
                            setEditing(null);
                          })
                        }
                      >
                        {tr(lang, "Zapisz", "Save")}
                      </Button>
                    </div>
                  ) : (
                    <div className="flex items-center gap-3">
                      <div className="min-w-0 flex-1">
                        <div className="text-[13px] text-white/90">{f.text}</div>
                        <div className="mt-0.5 flex gap-2 text-[10.5px] text-[var(--soul-muted)]">
                          <span className="rounded-full bg-white/[0.06] px-1.5">{catLabel(f.category, lang)}</span>
                          <span>{when(f.updatedAt, lang)}</span>
                          {f.source === "auto" && <span>· {tr(lang, "auto", "auto")}</span>}
                          {f.useCount > 0 && <span>· {tr(lang, `użyto ${f.useCount}×`, `used ${f.useCount}×`)}</span>}
                        </div>
                      </div>
                      <button
                        type="button"
                        onClick={() => {
                          setEditing(f.id);
                          setDraft({ text: f.text, category: f.category });
                        }}
                        className="rounded-full px-2 py-1 text-[11.5px] text-white/50 hover:bg-white/5 hover:text-white"
                      >
                        {tr(lang, "Edytuj", "Edit")}
                      </button>
                      <button
                        type="button"
                        onClick={() => void run(() => invoke("memory_delete", { id: f.id }))}
                        className="rounded-full px-2 py-1 text-[11.5px] text-[var(--soul-danger)]/80 hover:bg-[var(--soul-danger)]/10"
                      >
                        {tr(lang, "Usuń", "Delete")}
                      </button>
                    </div>
                  )}
                </motion.li>
              ))}
            </AnimatePresence>
          </ul>
        )}
      </Card>

      <DictionaryCard s={s} />
    </>
  );
}

/** Dictation dictionary, word replacements and voice snippets (saved with the Save bar). */
function DictionaryCard({ s }: P) {
  const lang = useAnim().cfg.uiLang;
  const { form, patch } = s;
  const words = form.customWords ?? [];
  const rules = form.replacements ?? [];
  const snippets = form.snippets ?? [];
  const [word, setWord] = useState("");
  const [rule, setRule] = useState({ from: "", to: "" });
  const [snippet, setSnippet] = useState({ trigger: "", text: "" });
  const input = "soul-input min-w-0 flex-1 px-3 py-2 text-[13px]";
  const chip = "flex items-center gap-2 rounded-xl bg-white/[0.05] px-3 py-1.5 text-[12.5px] text-white/85";
  const remove = (onClick: () => void) => (
    <button type="button" onClick={onClick} className="ml-auto shrink-0 text-[11.5px] text-[var(--soul-danger)]/80 hover:text-[var(--soul-danger)]">
      {tr(lang, "Usuń", "Delete")}
    </button>
  );

  const addWord = () => {
    const w = word.trim();
    if (!w || words.includes(w)) return;
    patch({ customWords: [...words, w] });
    setWord("");
  };
  const addRule = () => {
    if (!rule.from.trim() || !rule.to.trim()) return;
    patch({ replacements: [...rules, { from: rule.from.trim(), to: rule.to.trim() }] });
    setRule({ from: "", to: "" });
  };
  const addSnippet = () => {
    if (!snippet.trigger.trim() || !snippet.text.trim()) return;
    patch({ snippets: [...snippets, { trigger: snippet.trigger.trim(), text: snippet.text }] });
    setSnippet({ trigger: "", text: "" });
  };
  const onEnter = (fn: () => void) => (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      fn();
    }
  };

  return (
    <Card
      title={tr(lang, "Słownik i snippety", "Dictionary & snippets")}
      description={tr(
        lang,
        "Tylko dyktowanie. Słowa ze słownika są zawsze pisane dokładnie tak. Snippet: powiedz frazę, a Soul wklei cały tekst bez zmian.",
        "Dictation only. Dictionary words are always spelled exactly like this. Snippet: say the phrase and Soul pastes the whole text unchanged.",
      )}
    >
      <Row stacked label={tr(lang, "Słownik", "Dictionary")}>
        <div className="flex items-center gap-2">
          <input value={word} onChange={(e) => setWord(e.target.value)} onKeyDown={onEnter(addWord)} placeholder={tr(lang, "np. Supabase", "e.g. Supabase")} className={input} />
          <Button onClick={addWord} disabled={!word.trim()}>{tr(lang, "Dodaj", "Add")}</Button>
        </div>
        {words.length > 0 && (
          <div className="mt-2 flex flex-wrap gap-1.5">
            {words.map((w) => (
              <span key={w} className={chip}>
                {w}
                {remove(() => patch({ customWords: words.filter((x) => x !== w) }))}
              </span>
            ))}
          </div>
        )}
      </Row>

      <Row stacked label={tr(lang, "Zamiany", "Replacements")}>
        <div className="flex items-center gap-2">
          <input value={rule.from} onChange={(e) => setRule({ ...rule, from: e.target.value })} onKeyDown={onEnter(addRule)} placeholder={tr(lang, "słyszę: cloud code", "heard: cloud code")} className={input} />
          <span className="text-white/40">→</span>
          <input value={rule.to} onChange={(e) => setRule({ ...rule, to: e.target.value })} onKeyDown={onEnter(addRule)} placeholder={tr(lang, "piszę: Claude Code", "write: Claude Code")} className={input} />
          <Button onClick={addRule} disabled={!rule.from.trim() || !rule.to.trim()}>{tr(lang, "Dodaj", "Add")}</Button>
        </div>
        {rules.length > 0 && (
          <ul className="mt-2 flex flex-col gap-1">
            {rules.map((r, i) => (
              <li key={`${r.from}-${i}`} className={chip}>
                <span className="truncate">{r.from} → {r.to}</span>
                {remove(() => patch({ replacements: rules.filter((_, j) => j !== i) }))}
              </li>
            ))}
          </ul>
        )}
      </Row>

      <Row stacked label={tr(lang, "Snippety", "Snippets")}>
        <div className="flex flex-col gap-2">
          <input value={snippet.trigger} onChange={(e) => setSnippet({ ...snippet, trigger: e.target.value })} placeholder={tr(lang, "fraza, np. mój podpis", "phrase, e.g. my signature")} className={input} />
          <textarea value={snippet.text} onChange={(e) => setSnippet({ ...snippet, text: e.target.value })} rows={3} placeholder={tr(lang, "Tekst do wklejenia", "Text to paste")} className={`${input} resize-y`} />
          <div className="flex justify-end">
            <Button onClick={addSnippet} disabled={!snippet.trigger.trim() || !snippet.text.trim()}>{tr(lang, "Dodaj snippet", "Add snippet")}</Button>
          </div>
        </div>
        {snippets.length > 0 && (
          <ul className="mt-2 flex flex-col gap-1">
            {snippets.map((sn, i) => (
              <li key={`${sn.trigger}-${i}`} className={chip}>
                <span className="shrink-0 font-medium">„{sn.trigger}”</span>
                <span className="truncate text-white/50">{sn.text.replace(/\s+/g, " ")}</span>
                {remove(() => patch({ snippets: snippets.filter((_, j) => j !== i) }))}
              </li>
            ))}
          </ul>
        )}
      </Row>
    </Card>
  );
}

// ───────────────────────────── Sceny ─────────────────────────────
const EMPTY_SCENE: Scene = { id: 0, name: "", triggers: [], steps: [{ type: "open_app", value: "" }] };

export function ScenesPage({ s }: P) {
  const lang = useAnim().cfg.uiLang;
  const [scenes, setScenes] = useState<Scene[]>([]);
  const [draft, setDraft] = useState<Scene | null>(null);
  const [triggersText, setTriggersText] = useState("");

  const reload = useCallback(async () => {
    try {
      setScenes(await invoke<Scene[]>("scenes_list"));
    } catch (e) {
      s.setError(String(e));
    }
  }, [s]);

  useEffect(() => {
    void reload();
    let alive = true;
    const unsub = listen("soul://scenes-changed", () => void reload()).then((u) => (alive ? u : (u(), undefined)));
    return () => {
      alive = false;
      void unsub.then((u) => u?.());
    };
  }, [reload]);

  const edit = (scene: Scene) => {
    setDraft(JSON.parse(JSON.stringify(scene)) as Scene);
    setTriggersText(scene.triggers.join(", "));
  };

  const setStep = (i: number, p: Partial<SceneStep>) =>
    setDraft((d) => (d ? { ...d, steps: d.steps.map((st, j) => (j === i ? { ...st, ...p } : st)) } : d));
  const moveStep = (i: number, dir: -1 | 1) =>
    setDraft((d) => {
      if (!d) return d;
      const j = i + dir;
      if (j < 0 || j >= d.steps.length) return d;
      const steps = [...d.steps];
      [steps[i], steps[j]] = [steps[j], steps[i]];
      return { ...d, steps };
    });

  const save = async () => {
    if (!draft) return;
    try {
      const triggers = triggersText
        .split(/[,;\n]/)
        .map((t) => t.trim())
        .filter(Boolean);
      await invoke("scene_save", { scene: { ...draft, triggers } });
      setDraft(null);
      await reload();
    } catch (e) {
      s.setError(String(e));
    }
  };

  const stepLabel = (st: SceneStep) => {
    const t = STEP_TYPES.find((x) => x.id === st.type);
    const name = t ? (lang === "en" ? t.en : t.pl) : st.type;
    return st.type === "dnd" ? name : `${name}: ${st.value}`;
  };

  return (
    <>
      <Card
        title={tr(lang, "Sceny", "Scenes")}
        description={tr(
          lang,
          "Kilka akcji jednym poleceniem, np. „gramy w FiveM” → Discord, FiveM, playlista Spotify, głośność 40%, Nie przeszkadzać. Głosem: „stwórz scenę granie: otwórz Discord, FiveM i Spotify”, „uruchom scenę granie”.",
          "Several actions in one command, e.g. “gaming” → Discord, FiveM, a Spotify playlist, volume 40%, Do not disturb. By voice: “create scene gaming: open Discord, FiveM and Spotify”, “run scene gaming”.",
        )}
        action={
          !draft && (
            <Button variant="primary" onClick={() => edit(EMPTY_SCENE)}>
              + {tr(lang, "Nowa scena", "New scene")}
            </Button>
          )
        }
      >
        {scenes.length === 0 && !draft && (
          <p className="py-4 text-center text-[12px] text-[var(--soul-muted)]">
            {tr(
              lang,
              "Brak scen. Soul zaproponuje scenę, gdy kilka razy otworzysz te same aplikacje razem.",
              "No scenes yet. Soul suggests one when you open the same apps together a few times.",
            )}
          </p>
        )}
        <ul className="flex flex-col gap-2">
          {scenes.map((sc) => (
            <li key={sc.id} className="rounded-2xl border border-white/[0.06] bg-white/[0.03] px-4 py-3">
              <div className="flex items-center gap-3">
                <div className="min-w-0 flex-1">
                  <div className="text-[13.5px] font-medium">{sc.name}</div>
                  <div className="mt-0.5 truncate text-[11px] text-[var(--soul-muted)]">
                    {sc.triggers.length > 0
                      ? `„${sc.triggers.join("”, „")}”`
                      : tr(lang, `„uruchom scenę ${sc.name}”`, `“run scene ${sc.name}”`)}
                  </div>
                </div>
                <Button onClick={() => invoke("scene_run", { id: sc.id }).catch((e) => s.setError(String(e)))}>
                  ▶ {tr(lang, "Uruchom", "Run")}
                </Button>
                <Button onClick={() => edit(sc)}>{tr(lang, "Edytuj", "Edit")}</Button>
                <Button
                  variant="danger"
                  onClick={() =>
                    invoke("scene_delete", { id: sc.id })
                      .then(reload)
                      .catch((e) => s.setError(String(e)))
                  }
                >
                  ✕
                </Button>
              </div>
              <div className="mt-2 flex flex-wrap gap-1">
                {sc.steps.map((st, i) => (
                  <span key={i} className="rounded-full border border-white/10 bg-white/[0.05] px-2 py-0.5 text-[10.5px] text-white/70">
                    {i + 1}. {stepLabel(st)}
                  </span>
                ))}
              </div>
            </li>
          ))}
        </ul>
      </Card>

      <AnimatePresence>
        {draft && (
          <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: 8 }}>
            <Card
              title={draft.id ? tr(lang, "Edytuj scenę", "Edit scene") : tr(lang, "Nowa scena", "New scene")}
              action={
                <div className="flex gap-1.5">
                  <Button onClick={() => setDraft(null)}>{tr(lang, "Anuluj", "Cancel")}</Button>
                  <Button variant="primary" disabled={!draft.name.trim() || draft.steps.length === 0} onClick={() => void save()}>
                    {tr(lang, "Zapisz scenę", "Save scene")}
                  </Button>
                </div>
              }
            >
              <Row label={tr(lang, "Nazwa", "Name")}>
                <input
                  value={draft.name}
                  onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                  placeholder={tr(lang, "granie", "gaming")}
                  className="soul-input w-[280px] px-3 py-2 text-[13px]"
                />
              </Row>
              <Row
                label={tr(lang, "Frazy wywołujące", "Trigger phrases")}
                hint={tr(lang, "Oddzielone przecinkami, np. „gramy w FiveM, czas na granie”.", "Comma-separated, e.g. “gaming time, let's play FiveM”.")}
              >
                <input
                  value={triggersText}
                  onChange={(e) => setTriggersText(e.target.value)}
                  placeholder={tr(lang, "gramy w FiveM", "let's play FiveM")}
                  className="soul-input w-[280px] px-3 py-2 text-[13px]"
                />
              </Row>
              <div className="flex flex-col gap-1.5">
                <div className="text-[12px] text-white/60">{tr(lang, "Kroki (po kolei)", "Steps (in order)")}</div>
                {draft.steps.map((st, i) => {
                  const meta = STEP_TYPES.find((x) => x.id === st.type);
                  return (
                    <div key={i} className="flex items-center gap-2">
                      <span className="w-5 text-right text-[11px] tabular-nums text-white/40">{i + 1}.</span>
                      <Select
                        value={st.type}
                        onChange={(v) => setStep(i, { type: v as SceneStepType, value: v === "dnd" ? "on" : v === "media" ? "play" : "" })}
                        className="w-[180px]"
                      >
                        {STEP_TYPES.map((t) => (
                          <option key={t.id} value={t.id}>
                            {lang === "en" ? t.en : t.pl}
                          </option>
                        ))}
                      </Select>
                      {st.type === "dnd" ? (
                        <Select value={st.value || "on"} onChange={(value) => setStep(i, { value })} className="min-w-0 flex-1">
                          <option value="on">{tr(lang, "Włącz", "On")}</option>
                          <option value="off">{tr(lang, "Wyłącz", "Off")}</option>
                        </Select>
                      ) : st.type === "media" ? (
                        <Select value={st.value || "play"} onChange={(value) => setStep(i, { value })} className="min-w-0 flex-1">
                          <option value="play">play</option>
                          <option value="pause">pause</option>
                          <option value="next">next</option>
                          <option value="prev">prev</option>
                        </Select>
                      ) : (
                        <input
                          value={st.value}
                          onChange={(e) => setStep(i, { value: e.target.value })}
                          placeholder={meta?.placeholder}
                          inputMode={st.type === "delay" || st.type === "set_volume" ? "numeric" : undefined}
                          className="soul-input min-w-0 flex-1 px-3 py-2 text-[13px]"
                        />
                      )}
                      <button type="button" onClick={() => moveStep(i, -1)} className="px-1 text-white/40 hover:text-white" aria-label="↑">
                        ↑
                      </button>
                      <button type="button" onClick={() => moveStep(i, 1)} className="px-1 text-white/40 hover:text-white" aria-label="↓">
                        ↓
                      </button>
                      <button
                        type="button"
                        onClick={() => setDraft({ ...draft, steps: draft.steps.filter((_, j) => j !== i) })}
                        className="px-1 text-[var(--soul-danger)]/70 hover:text-[var(--soul-danger)]"
                        aria-label={tr(lang, "Usuń krok", "Remove step")}
                      >
                        ✕
                      </button>
                    </div>
                  );
                })}
                <div className="flex gap-1.5 pl-7 pt-1">
                  <Button onClick={() => setDraft({ ...draft, steps: [...draft.steps, { type: "open_app", value: "" }] })}>
                    + {tr(lang, "Krok", "Step")}
                  </Button>
                  <Button onClick={() => setDraft({ ...draft, steps: [...draft.steps, { type: "delay", value: "2000" }] })}>
                    + {tr(lang, "Opóźnienie", "Delay")}
                  </Button>
                </div>
              </div>
              <Callout tone="info">
                {tr(
                  lang,
                  "„Nie przeszkadzać” wycisza banery powiadomień (Windows: powiadomienia toast, Linux: GNOME/dunst).",
                  "“Do not disturb” silences notification banners (Windows toasts, Linux GNOME/dunst).",
                )}
              </Callout>
            </Card>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
