import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAnim } from "../anim/AnimationProvider";
import { tr, type UiLang } from "../i18n";
import { Button, Callout, Card, Row, Slider, Toggle } from "./ui";

/** Mirrors `wake::WakeStatus` (Rust). */
type WakeStatus = {
  enabled: boolean;
  trained: boolean;
  listening: boolean;
  paused: string | null;
  testing: boolean;
  samples: number;
  sensitivity: number;
  pauseInGames: boolean;
  error: string | null;
};

type Slot = { state: "empty" | "recording" | "ok" | "error"; ms?: number; error?: string; score?: number };
type TrainResult = { samples: number; scores: number[]; threshold: number };

const MIN_SAMPLES = 6;
const MAX_SAMPLES = 8;

function errorText(lang: UiLang, e: unknown): string {
  const code = String(e);
  switch (code) {
    case "busy":
      return tr(lang, "Soul jest zajęty — spróbuj za chwilę.", "Soul is busy — try again in a moment.");
    case "silence":
      return tr(lang, "Nic nie słychać. Mów bliżej mikrofonu.", "Nothing heard. Speak closer to the mic.");
    case "quiet":
      return tr(lang, "Za cicho — powiedz „Soul” głośniej.", "Too quiet — say “Soul” louder.");
    case "short":
      return tr(lang, "Za krótko — powiedz całe słowo „Soul”.", "Too short — say the whole word “Soul”.");
    case "long":
      return tr(lang, "Za długo — powiedz samo „Soul”, bez innych słów.", "Too long — say just “Soul”, nothing else.");
    case "untrained":
      return tr(lang, "Najpierw nagraj i zbuduj słowo-klucz.", "Record and build the wake word first.");
    default:
      return code;
  }
}

function pausedText(lang: UiLang, reason: string | null): string {
  const map: Record<string, [string, string]> = {
    disabled: ["wyłączone", "off"],
    untrained: ["brak wytrenowanego słowa", "not trained"],
    speaking: ["Soul mówi", "Soul is speaking"],
    recording: ["trwa nagrywanie", "recording"],
    busy: ["Soul pracuje", "Soul is working"],
    ask: ["Circle-to-Ask otwarte", "Circle-to-Ask open"],
    fullscreen: ["aplikacja pełnoekranowa / gra", "fullscreen app / game"],
    cooldown: ["chwila przerwy po wykryciu", "short cooldown after a detection"],
    mic: ["mikrofon niedostępny", "microphone unavailable"],
  };
  const m = reason ? map[reason] : null;
  return m ? tr(lang, m[0], m[1]) : (reason ?? "");
}

/** Settings → Głos → Słowo-klucz: enable, sensitivity, games pause, training wizard, live test. */
export function WakeWordCard() {
  const anim = useAnim();
  const lang = anim.cfg.uiLang;
  const [st, setSt] = useState<WakeStatus | null>(null);
  const [sensitivity, setSensitivity] = useState(0.5);
  const [error, setError] = useState<string | null>(null);
  const [wizard, setWizard] = useState(false);
  const [slots, setSlots] = useState<Slot[]>(() => Array.from({ length: MAX_SAMPLES }, () => ({ state: "empty" })));
  const [building, setBuilding] = useState(false);
  const [result, setResult] = useState<TrainResult | null>(null);
  const [testing, setTesting] = useState(false);
  const [score, setScore] = useState({ score: 0, threshold: 0.5 });
  const [hits, setHits] = useState(0);
  const [flash, setFlash] = useState(false);
  const saveTimer = useRef<number | undefined>(undefined);
  const testingRef = useRef(false);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    let alive = true;
    const on = <T,>(ev: string, cb: (p: T) => void) =>
      listen<T>(ev, (e) => cb(e.payload)).then((u) => (alive ? unsubs.push(u) : u()));
    invoke<WakeStatus>("wake_status")
      .then((s) => {
        if (!alive) return;
        setSt(s);
        setSensitivity(s.sensitivity);
        setSlots((prev) => prev.map((slot, i) => (i < s.samples && slot.state === "empty" ? { state: "ok" } : slot)));
      })
      .catch(() => undefined);
    on<WakeStatus>("soul://wake", (s) => {
      setSt(s);
      if (!s.testing && testingRef.current) {
        testingRef.current = false;
        setTesting(false);
      }
    });
    on<{ score: number; threshold: number; detected: boolean }>("soul://wake-score", (p) => {
      setScore({ score: p.score, threshold: p.threshold });
      if (p.detected) {
        setHits((n) => n + 1);
        setFlash(true);
        window.setTimeout(() => setFlash(false), 900);
      }
    });
    return () => {
      alive = false;
      unsubs.forEach((u) => u());
      if (testingRef.current) invoke("wake_test", { on: false }).catch(() => undefined);
    };
  }, []);

  const save = useCallback(
    async (patch: Partial<Pick<WakeStatus, "enabled" | "sensitivity" | "pauseInGames">>) => {
      if (!st) return;
      const next = { enabled: st.enabled, sensitivity, pauseInGames: st.pauseInGames, ...patch };
      setError(null);
      try {
        await invoke("wake_save_config", next);
      } catch (e) {
        setError(errorText(lang, e));
      }
    },
    [st, sensitivity, lang],
  );

  const onSensitivity = (v: number) => {
    setSensitivity(v);
    window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => void save({ sensitivity: v }), 300);
  };

  const record = async (index: number) => {
    setError(null);
    setSlots((prev) => prev.map((s, i) => (i === index ? { state: "recording" } : s)));
    try {
      const r = await invoke<{ index: number; durationMs: number }>("wake_train_record", { index });
      setSlots((prev) => prev.map((s, i) => (i === index ? { state: "ok", ms: r.durationMs } : s)));
      setResult(null);
    } catch (e) {
      setSlots((prev) => prev.map((s, i) => (i === index ? { state: "error", error: errorText(lang, e) } : s)));
    }
  };

  const recordNext = () => {
    const next = slots.findIndex((s) => s.state !== "ok");
    if (next >= 0) void record(next);
  };

  const build = async () => {
    setBuilding(true);
    setError(null);
    try {
      const r = await invoke<TrainResult>("wake_train_build");
      setResult(r);
      setSlots((prev) => prev.map((s, i) => ({ ...s, score: r.scores[i] })));
    } catch (e) {
      setError(errorText(lang, e));
    } finally {
      setBuilding(false);
    }
  };

  const resetSamples = async () => {
    await invoke("wake_train_reset", { model: false }).catch((e) => setError(errorText(lang, e)));
    setSlots(Array.from({ length: MAX_SAMPLES }, () => ({ state: "empty" })));
    setResult(null);
  };

  const toggleTest = async () => {
    const on = !testing;
    setError(null);
    try {
      await invoke("wake_test", { on });
      testingRef.current = on;
      setTesting(on);
      setHits(0);
      setScore((s) => ({ ...s, score: 0 }));
    } catch (e) {
      setError(errorText(lang, e));
    }
  };

  const okCount = slots.filter((s) => s.state === "ok").length;
  const anyRecording = slots.some((s) => s.state === "recording");
  const trained = st?.trained ?? false;
  const statusLine = !st
    ? ""
    : !st.enabled
      ? tr(lang, "Wyłączone", "Off")
      : st.listening
        ? tr(lang, "Nasłuchuję lokalnie…", "Listening on-device…")
        : `${tr(lang, "Wstrzymane", "Paused")}: ${pausedText(lang, st.paused)}`;

  return (
    <Card
      title={tr(lang, "Słowo-klucz „Soul”", "Wake word “Soul”")}
      description={tr(
        lang,
        "Powiedz „Soul”, a Soul zacznie słuchać — bez klawisza. Wykrywanie działa w 100% na komputerze; dźwięk trafia do Gemini dopiero po usłyszeniu słowa.",
        "Say “Soul” and Soul starts listening — no key needed. Detection runs 100% on this computer; audio reaches Gemini only after the word is heard.",
      )}
      action={
        <Toggle
          label={tr(lang, "Włącz słowo-klucz", "Enable wake word")}
          checked={st?.enabled ?? false}
          onChange={(enabled) => void save({ enabled })}
        />
      }
    >
      {error && <Callout tone="warn">{error}</Callout>}
      {st?.error && st.enabled && <Callout tone="warn">{st.error}</Callout>}
      {!trained && (
        <Callout>
          {tr(lang, "Zanim włączysz, nagraj 6–8 razy słowo „Soul” (przycisk niżej).", "Before enabling, record “Soul” 6–8 times (button below).")}
        </Callout>
      )}
      {st && trained && <div className="text-[12px] text-[var(--soul-muted)]">{statusLine}</div>}

      <Row
        label={tr(lang, "Czułość", "Sensitivity")}
        hint={tr(lang, "Wyżej = łatwiej wywołać, ale więcej fałszywych alarmów.", "Higher = easier to trigger, but more false alarms.")}
      >
        <Slider
          value={sensitivity}
          min={0}
          max={1}
          step={0.05}
          onChange={onSensitivity}
          format={(v) => `${Math.round(v * 100)}%`}
        />
      </Row>
      <Row
        label={tr(lang, "Wstrzymuj w grach", "Pause in games")}
        hint={tr(lang, "Nie nasłuchuje, gdy aktywna jest gra lub aplikacja pełnoekranowa.", "No listening while a game or fullscreen app is focused.")}
      >
        <Toggle checked={st?.pauseInGames ?? true} onChange={(pauseInGames) => void save({ pauseInGames })} />
      </Row>

      <div className="flex flex-wrap gap-2">
        <Button variant={wizard ? "ghost" : "primary"} onClick={() => setWizard((w) => !w)}>
          {wizard ? tr(lang, "Zamknij trening", "Close training") : tr(lang, "Trenuj słowo-klucz", "Train wake word")}
        </Button>
        <Button onClick={() => void toggleTest()} disabled={!trained}>
          {testing ? tr(lang, "Zatrzymaj test", "Stop test") : tr(lang, "Test", "Test")}
        </Button>
      </div>

      {testing && (
        <div className="rounded-2xl border border-white/[0.07] bg-black/20 p-3">
          <div className="mb-2 flex items-center justify-between text-[12px]">
            <span className="text-white/80">
              {tr(lang, "Powiedz „Soul”. Wynik na żywo:", "Say “Soul”. Live score:")}{" "}
              <span className="tabular-nums">{score.score.toFixed(2)}</span>
            </span>
            <span className="tabular-nums" style={{ color: flash ? "var(--soul-success)" : "var(--soul-muted)" }}>
              {flash ? tr(lang, "Wykryto!", "Detected!") : `${tr(lang, "wykryć", "detections")}: ${hits}`}
            </span>
          </div>
          <div className="relative h-2 overflow-hidden rounded-full bg-white/10">
            <div
              className="h-full rounded-full"
              style={{
                width: `${Math.min(100, score.score * 100)}%`,
                background: score.score >= score.threshold ? "var(--soul-success)" : "var(--soul-accent)",
                transition: `width ${anim.dur(90)}ms linear`,
              }}
            />
            <div className="absolute top-0 h-full w-0.5 bg-white/70" style={{ left: `${score.threshold * 100}%` }} />
          </div>
          <div className="mt-1.5 text-[11px] text-[var(--soul-muted)]">
            {tr(
              lang,
              "Biała kreska = próg. Test nie uruchamia nagrania i nic nie wysyła.",
              "White mark = threshold. The test never starts a turn or sends anything.",
            )}
          </div>
        </div>
      )}

      {wizard && (
        <div className="flex flex-col gap-3 rounded-2xl border border-white/[0.07] bg-black/20 p-3">
          <div className="text-[12px] leading-relaxed text-white/80">
            {tr(
              lang,
              `Kliknij „Nagraj”, potem powiedz raz „Soul” normalnym głosem (okno 1,8 s). Potrzeba ${MIN_SAMPLES}–${MAX_SAMPLES} próbek; mów tak, jak będziesz mówić na co dzień.`,
              `Click “Record”, then say “Soul” once in your normal voice (1.8 s window). ${MIN_SAMPLES}–${MAX_SAMPLES} samples needed; say it the way you will day to day.`,
            )}
          </div>
          <div className="grid grid-cols-4 gap-2">
            {slots.map((s, i) => {
              const weak = result && s.score !== undefined && s.score < result.threshold;
              return (
                <button
                  key={i}
                  type="button"
                  disabled={anyRecording}
                  onClick={() => void record(i)}
                  title={s.error ?? tr(lang, "Kliknij, aby nagrać ponownie", "Click to re-record")}
                  className={`rounded-xl border px-2 py-2 text-left text-[11.5px] transition-colors disabled:cursor-wait ${
                    s.state === "recording"
                      ? "border-[var(--soul-accent)] bg-[var(--soul-accent)]/15"
                      : s.state === "ok"
                        ? weak
                          ? "border-[var(--soul-dictation)]/50 bg-white/[0.04]"
                          : "border-[var(--soul-success)]/40 bg-white/[0.04]"
                        : s.state === "error"
                          ? "border-[var(--soul-danger)]/50 bg-white/[0.03]"
                          : "border-white/[0.07] hover:bg-white/[0.04]"
                  }`}
                >
                  <div className="font-medium text-white/90">#{i + 1}</div>
                  <div className="truncate text-[10.5px] text-[var(--soul-muted)]">
                    {s.state === "recording"
                      ? tr(lang, "Mów: „Soul”", "Say: “Soul”")
                      : s.state === "ok"
                        ? s.score !== undefined
                          ? `${tr(lang, "wynik", "score")} ${s.score.toFixed(2)}`
                          : s.ms
                            ? `${s.ms} ms`
                            : "✓"
                        : s.state === "error"
                          ? "✕"
                          : tr(lang, "pusta", "empty")}
                  </div>
                </button>
              );
            })}
          </div>
          {slots.find((s) => s.state === "error")?.error && (
            <div className="text-[11.5px] text-[var(--soul-danger)]">{slots.find((s) => s.state === "error")?.error}</div>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="primary" onClick={recordNext} disabled={anyRecording || okCount >= MAX_SAMPLES}>
              {anyRecording ? tr(lang, "Nagrywam…", "Recording…") : tr(lang, `Nagraj (${okCount}/${MAX_SAMPLES})`, `Record (${okCount}/${MAX_SAMPLES})`)}
            </Button>
            <Button onClick={() => void build()} disabled={anyRecording || building || okCount < MIN_SAMPLES}>
              {building ? tr(lang, "Buduję…", "Building…") : tr(lang, "Zbuduj słowo-klucz", "Build wake word")}
            </Button>
            <Button onClick={() => void resetSamples()} disabled={anyRecording || building}>
              {tr(lang, "Od nowa", "Start over")}
            </Button>
          </div>
          {result && (
            <div className="text-[11.5px] leading-relaxed text-[var(--soul-muted)]">
              {result.scores.every((x) => x >= result.threshold)
                ? tr(
                    lang,
                    `Gotowe: ${result.samples} próbek. Każda rozpoznana przez pozostałe — włącz przełącznik powyżej i użyj „Test”.`,
                    `Done: ${result.samples} samples. Each one is recognised by the others — enable the switch above and try “Test”.`,
                  )
                : tr(
                    lang,
                    `Zapisano (${result.samples} próbek). Próbki z wynikiem poniżej ${result.threshold.toFixed(2)} odstają — nagraj je ponownie (kliknij kafelek) i zbuduj jeszcze raz.`,
                    `Saved (${result.samples} samples). Samples scoring under ${result.threshold.toFixed(2)} are outliers — re-record them (click the tile) and build again.`,
                  )}
            </div>
          )}
        </div>
      )}
    </Card>
  );
}
