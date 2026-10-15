import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";
import { tr } from "../i18n";
import Spirit from "../spirit/Spirit";
import {
  FALLBACK_MODELS,
  INITIAL_FORM,
  isCtrlAltLetterHotkey,
  pickNewestFlashLite,
  pickPrimary,
  withDefaults,
  type GeminiModelInfo,
  type MicrophoneInfo,
  type SettingsData,
} from "./model";
import { AdvancedPage, GeneralPage, HotkeysPage, IslandPage, MicPage, ModelsPage, VoicePage } from "./pages";
import AppearancePage from "./AppearancePage";
import { MemoryPage, ScenesPage } from "./MemoryScenes";
import { Button } from "./ui";

export type PageId =
  | "general"
  | "mic"
  | "hotkeys"
  | "models"
  | "voice"
  | "memory"
  | "scenes"
  | "island"
  | "appearance"
  | "advanced";

/** Everything a page needs; built once per render in `Settings`. */
export interface SettingsCtx {
  form: SettingsData;
  patch: (p: Partial<SettingsData>) => void;
  setForm: React.Dispatch<React.SetStateAction<SettingsData>>;
  models: GeminiModelInfo[];
  loadingModels: boolean;
  refreshModels: (apiKey: string, model: string, fallback: string) => Promise<void>;
  mics: MicrophoneInfo[];
  loadingMics: boolean;
  refreshMics: (selected: string) => Promise<void>;
  restartMonitor: (mic: string) => Promise<string | null>;
  micNotice: string | null;
  modelNotice: string | null;
  dismissModelNotice: () => void;
  errorLog: string[];
  clearErrorLog: () => void;
  testKey: () => Promise<void>;
  testing: boolean;
  previewVoice: (voice?: string) => Promise<void>;
  previewing: string | null;
  setError: (e: string | null) => void;
}

const NAV: { id: PageId; pl: string; en: string; icon: string }[] = [
  { id: "general", pl: "Ogólne", en: "General", icon: "M12 3v2m0 14v2M3 12h2m14 0h2M5.6 5.6l1.4 1.4m10 10 1.4 1.4M5.6 18.4 7 17m10-10 1.4-1.4M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z" },
  { id: "mic", pl: "Mikrofon", en: "Microphone", icon: "M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3" },
  { id: "hotkeys", pl: "Skróty", en: "Hotkeys", icon: "M4 7h16v10H4zM7 10h1m3 0h1m3 0h1M8 14h8" },
  { id: "models", pl: "Modele", en: "Models", icon: "M12 3 4 7.5v9L12 21l8-4.5v-9L12 3zm0 0v18M4 7.5l8 4.5 8-4.5" },
  { id: "voice", pl: "Głos i dusza", en: "Voice & soul", icon: "M4 10v4M8 7v10M12 4v16M16 7v10M20 10v4" },
  { id: "memory", pl: "Pamięć", en: "Memory", icon: "M9 4a3 3 0 0 0-3 3v.5A3 3 0 0 0 4 10.3 3 3 0 0 0 5 15a3 3 0 0 0 4 4.5V4zm6 0a3 3 0 0 1 3 3v.5a3 3 0 0 1 2 2.8 3 3 0 0 1-1 4.7 3 3 0 0 1-4 4.5V4z" },
  { id: "scenes", pl: "Sceny", en: "Scenes", icon: "M5 4l14 8-14 8V4z" },
  { id: "island", pl: "Wyspa", en: "Island", icon: "M6 6h12a3 3 0 0 1 0 6H6a3 3 0 0 1 0-6zM8 16h8" },
  { id: "appearance", pl: "Wygląd", en: "Appearance", icon: "M12 3a9 9 0 1 0 0 18c1 0 1.5-.8 1.5-1.6 0-1.3-1-1.4-1-2.6 0-.9.7-1.6 1.6-1.6H16a5 5 0 0 0 5-5C21 6.5 17 3 12 3zM7.5 12a1 1 0 1 0 0-.1M10 8a1 1 0 1 0 0-.1M15 8a1 1 0 1 0 0-.1" },
  { id: "advanced", pl: "Zaawansowane", en: "Advanced", icon: "M4 6h10M18 6h2M4 12h4m4 0h8M4 18h12m4 0h0M14 4v4M8 10v4M16 16v4" },
];

export default function Settings() {
  const anim = useAnim();
  const lang = anim.cfg.uiLang;
  const [page, setPage] = useState<PageId>("general");
  const [form, setForm] = useState<SettingsData>(INITIAL_FORM);
  const saved = useRef<string>(JSON.stringify(INITIAL_FORM));
  const [savedTick, setSavedTick] = useState(0);
  const [previewing, setPreviewing] = useState<string | null>(null);
  const [models, setModels] = useState<GeminiModelInfo[]>(FALLBACK_MODELS);
  const [mics, setMics] = useState<MicrophoneInfo[]>([]);
  const [micNotice, setMicNotice] = useState<string | null>(null);
  const [modelNotice, setModelNotice] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  const [loadingMics, setLoadingMics] = useState(false);
  const [errorLog, setErrorLog] = useState<string[]>([]);

  const patch = useCallback((p: Partial<SettingsData>) => setForm((prev) => ({ ...prev, ...p })), []);

  const restartMonitor = useCallback(async (microphone: string) => {
    try {
      const name = await invoke<string>("start_mic_monitor", { device: microphone || null });
      setMicNotice(null);
      return name;
    } catch (err) {
      setMicNotice(String(err));
      return null;
    }
  }, []);

  const refreshMics = useCallback(async (selected: string) => {
    setLoadingMics(true);
    try {
      const list = await invoke<MicrophoneInfo[]>("list_microphones");
      setMics(list);
      if (selected && !list.some((m) => m.name === selected)) {
        setMicNotice(
          lang === "en"
            ? `Saved microphone "${selected}" is not connected — using the system default until you pick another.`
            : `Zapisany mikrofon „${selected}” nie jest podłączony — używam domyślnego, dopóki nie wybierzesz innego.`,
        );
        setForm((prev) => ({ ...prev, microphone: "" }));
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoadingMics(false);
    }
  }, [lang]);

  const refreshModels = useCallback(async (apiKey: string, currentModel: string, currentFallback: string) => {
    if (!apiKey.trim()) {
      setModels(FALLBACK_MODELS);
      return;
    }
    setLoadingModels(true);
    try {
      const list = await invoke<GeminiModelInfo[]>("list_gemini_models", { apiKey });
      const next = list.length > 0 ? list : FALLBACK_MODELS;
      setModels(next);
      setForm((prev) => {
        const retired = prev.retiredModels ?? [];
        let model = currentModel || prev.model;
        let fallbackModel = currentFallback || prev.fallbackModel;
        if (!next.some((m) => m.id === model) || retired.includes(model)) model = pickPrimary(next, retired);
        if (
          !fallbackModel ||
          fallbackModel === model ||
          !next.some((m) => m.id === fallbackModel) ||
          retired.includes(fallbackModel)
        ) {
          fallbackModel = pickNewestFlashLite(model, next, retired);
        }
        return { ...prev, model, fallbackModel };
      });
    } catch (err) {
      setModels(FALLBACK_MODELS);
      setError(String(err));
    } finally {
      setLoadingModels(false);
    }
  }, []);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    let alive = true;
    const on = <T,>(ev: string, cb: (p: T) => void) =>
      listen<T>(ev, (e) => cb(e.payload)).then((u) => (alive ? unsubs.push(u) : u()));

    on<{ ok: boolean; message?: string | null }>("soul://voice-preview", (p) => {
      if (!p.ok) {
        setPreviewing(null);
        setError(`Voice preview: ${p.message ?? "unknown error"}`);
      } else {
        // All audio is queued; `voice-speaking: false` normally clears this sooner.
        window.setTimeout(() => setPreviewing(null), 12_000);
      }
    });
    // Tray → "Listen for Soul" without a trained wake word opens the Voice page.
    on<string>("settings://navigate", (p) => {
      if (NAV.some((n) => n.id === p)) setPage(p as PageId);
    });
    on<{ speaking: boolean }>("soul://voice-speaking", (p) => {
      if (!p.speaking) setPreviewing(null);
    });

    invoke<SettingsData>("get_settings")
      .then(async (settings) => {
        const next = withDefaults(settings);
        // Appearance is managed live by AnimationProvider; keep it out of the form.
        delete (next as Partial<SettingsData> & { appearance?: unknown }).appearance;
        setForm(next);
        saved.current = JSON.stringify(next);
        setSavedTick((n) => n + 1);
        if (next.modelNotice) setModelNotice(next.modelNotice);
        await refreshModels(next.apiKey, next.model, next.fallbackModel);
        await refreshMics(next.microphone);
        // Initial auto-adjustments (model healing, missing mic) are the baseline, not "unsaved".
        setForm((f) => {
          saved.current = JSON.stringify(f);
          return f;
        });
        setSavedTick((n) => n + 1);
      })
      .catch((e) => setError(String(e)));

    invoke<string[]>("get_error_log").then(setErrorLog).catch(() => undefined);
    on<{ message: string }>("soul://error-log", (p) => setErrorLog((prev) => [p.message, ...prev].slice(0, 30)));
    on<{ message: string }>("soul://mic-notice", (p) => setMicNotice(p.message));
    on<number>("soul://voice-volume", (v) => {
      setForm((f) => ({ ...f, voiceVolume: v }));
      try {
        saved.current = JSON.stringify({ ...(JSON.parse(saved.current) as SettingsData), voiceVolume: v });
      } catch {
        /* ignore */
      }
      setSavedTick((n) => n + 1);
    });
    on<{ message: string }>("soul://model-notice", (p) => {
      setModelNotice(p.message);
      invoke<SettingsData>("get_settings")
        .then((s) =>
          setForm((prev) => ({
            ...prev,
            model: s.model,
            fallbackModel: s.fallbackModel ?? "",
            retiredModels: s.retiredModels ?? [],
            modelNotice: s.modelNotice ?? p.message,
          })),
        )
        .catch(() => undefined);
    });

    return () => {
      alive = false;
      unsubs.forEach((u) => u());
      invoke("stop_mic_monitor").catch(() => undefined);
    };
  }, [refreshModels, refreshMics]);

  const dirty = useMemo(() => JSON.stringify(form) !== saved.current, [form, savedTick]); // eslint-disable-line react-hooks/exhaustive-deps

  const hotkeyBlocked =
    isCtrlAltLetterHotkey(form.askHotkey) ||
    (form.talkMode === "separate" && (isCtrlAltLetterHotkey(form.hotkey) || isCtrlAltLetterHotkey(form.agentHotkey)));

  async function onSave(e?: FormEvent) {
    e?.preventDefault();
    setSaving(true);
    setError(null);
    setStatus(null);
    try {
      await invoke("save_app_settings", { settings: form });
      saved.current = JSON.stringify(form);
      setSavedTick((n) => n + 1);
      setStatus(tr(lang, "Zapisano", "Saved"));
      window.setTimeout(() => setStatus(null), 1800);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  function onDiscard() {
    setForm(JSON.parse(saved.current) as SettingsData);
  }

  const previewVoice = useCallback(
    async (voice?: string) => {
      const v = voice ?? form.voiceName ?? "Charon";
      setPreviewing(v);
      setError(null);
      try {
        await invoke("voice_preview", {
          voice: v,
          speed: form.voiceSpeed ?? "normal",
          personality: form.personality ?? "neutral",
        });
      } catch (err) {
        setPreviewing(null);
        setError(String(err));
      }
    },
    [form.voiceName, form.voiceSpeed, form.personality],
  );

  const testKey = useCallback(async () => {
    setTesting(true);
    setError(null);
    setStatus(null);
    try {
      const msg = await invoke<string>("test_api_key", { apiKey: form.apiKey, model: form.model });
      setStatus(msg);
      await refreshModels(form.apiKey, form.model, form.fallbackModel);
    } catch (err) {
      setError(String(err));
    } finally {
      setTesting(false);
    }
  }, [form.apiKey, form.model, form.fallbackModel, refreshModels]);

  const ctx: SettingsCtx = {
    form,
    patch,
    setForm,
    models,
    loadingModels,
    refreshModels,
    mics,
    loadingMics,
    refreshMics,
    restartMonitor,
    micNotice,
    modelNotice,
    dismissModelNotice: () => {
      setModelNotice(null);
      patch({ modelNotice: null });
      invoke("clear_model_notice").catch(() => undefined);
    },
    errorLog,
    clearErrorLog: () => {
      invoke("clear_error_log").catch(() => undefined);
      setErrorLog([]);
    },
    testKey,
    testing,
    previewVoice,
    previewing,
    setError,
  };

  const mainRef = useRef<HTMLElement>(null);
  useEffect(() => {
    mainRef.current?.scrollTo({ top: 0 });
  }, [page]);

  const current = NAV.find((n) => n.id === page)!;
  const pageMotion = {
    initial: { opacity: 0, y: 10 },
    animate: { opacity: 1, y: 0 },
    exit: { opacity: 0, y: -6 },
    transition: { duration: anim.dur(180) },
  };

  return (
    <div
      className="flex h-full text-white"
      style={{
        background:
          "radial-gradient(1200px 600px at 85% -10%, rgb(var(--soul-accent-rgb) / 0.10), transparent 60%), #0e0e11",
      }}
    >
      <aside className="flex w-[228px] shrink-0 flex-col border-r border-white/[0.06] bg-black/20 px-3 py-4">
        <div className="mb-5 flex items-center gap-2 px-2">
          <Spirit state={previewing ? "speaking" : "idle"} color={anim.cfg.accent} size={22} />
          <div>
            <div className="text-[15px] font-semibold tracking-tight">Soul</div>
            <div className="text-[10.5px] text-[var(--soul-muted)]">{tr(lang, "Ustawienia", "Settings")}</div>
          </div>
        </div>
        <nav className="flex flex-col gap-0.5">
          {NAV.map((n) => {
            const active = n.id === page;
            return (
              <button
                key={n.id}
                type="button"
                onClick={() => setPage(n.id)}
                className={`relative flex items-center gap-2.5 rounded-xl px-3 py-2 text-left text-[13px] transition-colors ${
                  active ? "text-white" : "text-white/55 hover:bg-white/[0.04] hover:text-white/85"
                }`}
              >
                {active && (
                  <motion.span
                    layoutId={anim.on("tabs") ? "nav-pill" : undefined}
                    transition={anim.spring("ui")}
                    className="absolute inset-0 rounded-xl bg-white/[0.09] shadow-[inset_0_1px_0_rgba(255,255,255,0.06)]"
                  />
                )}
                <svg
                  className="relative shrink-0"
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke={active ? "var(--soul-accent)" : "currentColor"}
                  strokeWidth="1.7"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d={n.icon} />
                </svg>
                <span className="relative">{lang === "en" ? n.en : n.pl}</span>
              </button>
            );
          })}
        </nav>
        <div className="mt-auto px-2 text-[10.5px] leading-relaxed text-white/30">
          Soul 0.1 · {tr(lang, "Zmiany wyglądu zapisują się od razu", "Appearance changes save instantly")}
        </div>
      </aside>

      <main ref={mainRef} className="soul-scroll relative min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-[720px] px-8 pb-28 pt-7">
          <AnimatePresence mode="wait" initial={false}>
            <motion.div key={page} {...pageMotion}>
              <h1 className="mb-1 text-[22px] font-semibold tracking-tight">{lang === "en" ? current.en : current.pl}</h1>
              <div className="mb-6 h-px bg-gradient-to-r from-white/10 to-transparent" />
              <div className="flex flex-col gap-4">
                {page === "general" && <GeneralPage s={ctx} />}
                {page === "mic" && <MicPage s={ctx} />}
                {page === "hotkeys" && <HotkeysPage s={ctx} />}
                {page === "models" && <ModelsPage s={ctx} />}
                {page === "voice" && <VoicePage s={ctx} />}
                {page === "memory" && <MemoryPage s={ctx} />}
                {page === "scenes" && <ScenesPage s={ctx} />}
                {page === "island" && <IslandPage s={ctx} />}
                {page === "appearance" && <AppearancePage s={ctx} />}
                {page === "advanced" && <AdvancedPage s={ctx} />}
              </div>
            </motion.div>
          </AnimatePresence>
        </div>

        {/* Floating save bar for backend settings (appearance autosaves). */}
        <AnimatePresence>
          {(dirty || status || error) && (
            <motion.div
              initial={{ opacity: 0, y: 24, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 24, scale: 0.98 }}
              transition={anim.spring("morph")}
              className="island-glass fixed bottom-5 left-[calc(50%+114px)] z-50 flex w-[min(620px,calc(100vw-280px))] -translate-x-1/2 items-center gap-3 rounded-full py-2 pl-5 pr-2"
            >
              <span
                className="min-w-0 flex-1 truncate text-[12.5px]"
                style={{ color: error ? "var(--soul-danger)" : status && !dirty ? "var(--soul-success)" : undefined }}
                title={error ?? undefined}
              >
                {error ?? (dirty ? tr(lang, "Masz niezapisane zmiany", "You have unsaved changes") : status)}
              </span>
              {dirty && (
                <>
                  <Button onClick={onDiscard}>{tr(lang, "Odrzuć", "Discard")}</Button>
                  <Button variant="primary" onClick={() => void onSave()} disabled={saving || hotkeyBlocked}>
                    {saving ? tr(lang, "Zapisuję…", "Saving…") : tr(lang, "Zapisz", "Save")}
                  </Button>
                </>
              )}
              {!dirty && error && <Button onClick={() => setError(null)}>OK</Button>}
            </motion.div>
          )}
        </AnimatePresence>
      </main>
    </div>
  );
}
