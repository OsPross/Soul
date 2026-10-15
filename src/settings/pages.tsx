import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { motion } from "framer-motion";
import { useAnim, useAppearance } from "../anim/AnimationProvider";
import { DEFAULT_APPEARANCE } from "../anim/config";
import { tr } from "../i18n";
import { readLevel, useLevels } from "../lib/levels";
import Spirit from "../spirit/Spirit";
import { tokens } from "../theme/tokens";
import HotkeyRecorder, { talkKeyLabel } from "./HotkeyRecorder";
import {
  DEFAULT_ASK,
  DEFAULT_ASK_FALLBACK,
  DEFAULT_COMMANDS,
  DEFAULT_DICTATION,
  isAskModel,
  isCommandsLive,
  isCtrlAltLetterHotkey,
  isTranscribeLive,
  PERSONALITIES,
  pickNewestFlashLite,
  VOICES,
  type IslandSections,
  type SettingsData,
} from "./model";
import type { SettingsCtx } from "./Settings";
import { Button, Callout, Card, Keycap, Row, Segmented, Select, Slider, Toggle } from "./ui";
import { WakeWordCard } from "./WakeWord";

type P = { s: SettingsCtx };

function useLang() {
  return useAnim().cfg.uiLang;
}

// ───────────────────────────── Ogólne ─────────────────────────────
export function GeneralPage({ s }: P) {
  const lang = useLang();
  const { anim, update } = useAppearance();
  const { form, patch } = s;
  const hasKey = form.apiKey.trim().length > 0;
  const [keyStore, setKeyStore] = useState<{ storage: string; store: string } | null>(null);
  useEffect(() => {
    invoke<{ storage: string; store: string }>("api_key_storage").then(setKeyStore).catch(() => undefined);
  }, [form.apiKey]);
  return (
    <>
      <Card>
        <div className="flex items-center gap-5">
          <Spirit state={hasKey ? "idle" : "thinking"} color={anim.cfg.accent} size={54} />
          <div>
            <div className="text-[17px] font-semibold tracking-tight">
              {hasKey ? tr(lang, "Cześć, tu Soul.", "Hi, I'm Soul.") : tr(lang, "Jeszcze chwila…", "Almost there…")}
            </div>
            <p className="mt-1 max-w-[440px] text-[12.5px] leading-relaxed text-[var(--soul-muted)]">
              {hasKey ? (
                <>
                  {tr(lang, "Przytrzymaj", "Hold")} <Keycap>{talkKeyLabel(form.talkKey ?? "ControlRight")}</Keycap>{" "}
                  {tr(
                    lang,
                    "i mów — tekst wklei się tam, gdzie piszesz. Zacznij od „Soul…” albo czasownika (otwórz, ścisz…), aby wydać polecenie.",
                    "and speak — text is pasted where you type. Start with “Soul…” or a verb (open, mute…) to run a command.",
                  )}
                </>
              ) : (
                tr(lang, "Wklej klucz Gemini API poniżej i zapisz, aby obudzić Soul.", "Paste your Gemini API key below and save to wake Soul up.")
              )}
            </p>
          </div>
        </div>
      </Card>

      <Card
        title={tr(lang, "Klucz Gemini API", "Gemini API key")}
        description={
          keyStore?.storage === "file"
            ? tr(
                lang,
                "Brak systemowego magazynu haseł — klucz jest w pliku ustawień w folderze danych aplikacji. Nigdy nie trafia do logów.",
                "No system credential store — the key is kept in the settings file in the app data folder. Never written to logs.",
              )
            : tr(
                lang,
                `Przechowywany lokalnie w: ${keyStore?.store ?? "magazyn poświadczeń systemu"}. Nigdy nie trafia do logów.`,
                `Stored locally in ${keyStore?.store ?? "the system credential store"}. Never written to logs.`,
              )
        }
      >
        <div className="flex gap-2">
          <input
            type="password"
            autoComplete="off"
            spellCheck={false}
            className="soul-input min-w-0 flex-1 px-3 py-2 text-[13px]"
            value={form.apiKey}
            onChange={(e) => patch({ apiKey: e.target.value })}
            onBlur={() => s.refreshModels(form.apiKey, form.model, form.fallbackModel)}
            placeholder="AIza… / AQ.…"
          />
          <Button onClick={() => void s.testKey()} disabled={s.testing || !form.apiKey}>
            {s.testing ? tr(lang, "Sprawdzam…", "Testing…") : tr(lang, "Testuj klucz", "Test key")}
          </Button>
        </div>
      </Card>

      <Card title={tr(lang, "Język", "Language")}>
        <Row
          label={tr(lang, "Język mowy", "Speech language")}
          hint={tr(lang, "Dla dyktowania, poleceń i odpowiedzi.", "For dictation, commands and replies.")}
        >
          <Select value={form.language} onChange={(language) => patch({ language })} className="w-[220px]">
            <option value="auto">{tr(lang, "Automatyczny", "Auto (keep speaker language)")}</option>
            <option value="pl">Polski</option>
            <option value="en">English</option>
            <option value="de">Deutsch</option>
            <option value="es">Español</option>
            <option value="fr">Français</option>
          </Select>
        </Row>
        <Row label={tr(lang, "Język interfejsu", "Interface language")} hint={tr(lang, "Zmienia się od razu.", "Applies instantly.")}>
          <Segmented
            value={anim.cfg.uiLang}
            options={[
              { value: "pl", label: "Polski" },
              { value: "en", label: "English" },
            ]}
            onChange={(uiLang) => update({ ...anim.cfg, uiLang })}
          />
        </Row>
      </Card>
    </>
  );
}

// ───────────────────────────── Mikrofon ─────────────────────────────
export function MicPage({ s }: P) {
  const lang = useLang();
  const anim = useAnim();
  const { form, patch, mics } = s;
  const [device, setDevice] = useState<string | null>(null);
  const restart = s.restartMonitor;

  // Level meter runs only while this page is visible (and restarts when the window refocuses).
  useEffect(() => {
    let alive = true;
    const start = () => restart(form.microphone).then((n) => alive && setDevice(n));
    void start();
    const unlisten = getCurrentWindow().onFocusChanged(({ payload }) => {
      if (payload) void start();
    });
    return () => {
      alive = false;
      void unlisten.then((u) => u());
      invoke("stop_mic_monitor").catch(() => undefined);
    };
  }, [form.microphone, restart]);

  const options = [{ name: "", isDefault: false }, ...mics];
  return (
    <>
      <Card>
        <div className="flex items-center gap-6">
          <Spirit state="listening" color={tokens.accentDictation} size={48} />
          <div className="min-w-0 flex-1">
            <div className="mb-1 text-[13px] font-medium">{tr(lang, "Powiedz coś — dusza słucha", "Say something — the spirit is listening")}</div>
            <div className="mb-3 truncate text-[11.5px] text-[var(--soul-muted)]">{device ?? "…"}</div>
            <LevelMeter color={tokens.accentDictation} />
          </div>
        </div>
      </Card>

      <Card
        title={tr(lang, "Urządzenie wejściowe", "Input device")}
        action={
          <Button onClick={() => void s.refreshMics(form.microphone)} disabled={s.loadingMics}>
            {s.loadingMics ? tr(lang, "Odświeżam…", "Refreshing…") : tr(lang, "Odśwież", "Refresh")}
          </Button>
        }
      >
        <div className="flex flex-col gap-1" role="radiogroup">
          {options.map((m) => {
            const active = form.microphone === m.name;
            return (
              <button
                key={m.name || "__default"}
                type="button"
                role="radio"
                aria-checked={active}
                onClick={() => patch({ microphone: m.name })}
                className={`relative flex items-center gap-3 rounded-xl px-3.5 py-2.5 text-left text-[13px] transition-colors ${
                  active ? "text-white" : "text-white/65 hover:bg-white/[0.04]"
                }`}
              >
                {active && (
                  <motion.span
                    layoutId={anim.on("tabs") ? "mic-sel" : undefined}
                    transition={anim.spring("ui")}
                    className="absolute inset-0 rounded-xl border border-[var(--soul-accent)]/40 bg-[var(--soul-accent)]/[0.08]"
                  />
                )}
                <span
                  className="relative flex h-4 w-4 shrink-0 items-center justify-center rounded-full border"
                  style={{ borderColor: active ? "var(--soul-accent)" : "rgba(255,255,255,0.25)" }}
                >
                  {active && <span className="h-2 w-2 rounded-full bg-[var(--soul-accent)]" />}
                </span>
                <span className="relative min-w-0 flex-1 truncate">
                  {m.name || tr(lang, "Domyślny systemowy", "System default")}
                </span>
                {m.isDefault && <span className="relative text-[10.5px] text-white/35">{tr(lang, "domyślny", "default")}</span>}
              </button>
            );
          })}
        </div>
        {s.micNotice && <Callout tone="warn">{s.micNotice}</Callout>}
      </Card>
    </>
  );
}

/** Mic level from `soul://mic-level` refs — rAF, transform-only. */
function LevelMeter({ color }: { color: string }) {
  useLevels();
  const fill = useRef<HTMLSpanElement>(null);
  const peak = useRef<HTMLSpanElement>(null);
  const label = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    let raf = 0;
    let p = 0;
    let shown = -1;
    const tick = () => {
      const l = Math.min(1, readLevel("mic"));
      p = Math.max(l, p - 0.008);
      if (fill.current) fill.current.style.transform = `scaleX(${l.toFixed(3)})`;
      if (peak.current) peak.current.style.transform = `translateX(${(p * 100).toFixed(1)}%)`;
      const pct = Math.round(l * 100);
      if (label.current && pct !== shown) {
        shown = pct;
        label.current.textContent = `${pct}%`;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);
  return (
    <div className="flex items-center gap-3">
      <div className="relative h-2.5 flex-1 overflow-hidden rounded-full bg-white/10">
        <span ref={fill} className="absolute inset-0 origin-left rounded-full" style={{ background: color, transform: "scaleX(0)" }} />
        <span ref={peak} className="absolute inset-y-0 left-0 w-full" style={{ transform: "translateX(0%)" }}>
          <span className="absolute -left-[1px] top-0 h-full w-[2px] bg-white/70" />
        </span>
      </div>
      <span ref={label} className="w-10 text-right text-[11.5px] tabular-nums text-white/60">
        0%
      </span>
    </div>
  );
}

// ───────────────────────────── Skróty ─────────────────────────────
export function HotkeysPage({ s }: P) {
  const lang = useLang();
  const { form, patch } = s;
  const smart = (form.talkMode ?? "smart") === "smart";
  const altgr = (hk: string) =>
    isCtrlAltLetterHotkey(hk) && (
      <Callout tone="danger">
        {tr(
          lang,
          "Niedozwolone: Ctrl+Alt+litera koliduje z polskim AltGr (np. ć). Użyj Ctrl+Win albo Win+Alt.",
          "Not allowed: Ctrl+Alt+letter collides with Polish AltGr (e.g. ć). Use Ctrl+Win or Win+Alt.",
        )}
      </Callout>
    );

  return (
    <>
      <Card
        title={tr(lang, "Tryb mówienia", "Talk mode")}
        action={
          <Segmented
            value={form.talkMode ?? "smart"}
            options={[
              { value: "smart", label: tr(lang, "Smart · jeden klawisz", "Smart · one key") },
              { value: "separate", label: tr(lang, "Osobne skróty", "Separate keys") },
            ]}
            onChange={(talkMode) => patch({ talkMode })}
          />
        }
      >
        {smart ? (
          <>
            <Row stacked label={tr(lang, "Przytrzymaj, aby mówić", "Hold to talk")}>
              <HotkeyRecorder mode="single" value={form.talkKey ?? "ControlRight"} onChange={(talkKey) => patch({ talkKey })} />
            </Row>
            <div className="grid grid-cols-3 gap-2 text-[11.5px] text-white/70">
              <Gesture keys={[talkKeyLabel(form.talkKey ?? "ControlRight")]} pl="przytrzymaj i mów → dyktowanie" en="hold & speak → dictation" />
              <Gesture keys={["2×", talkKeyLabel(form.talkKey ?? "ControlRight")]} pl="podwójnie + przytrzymaj → polecenie" en="double-tap & hold → command" />
              <Gesture keys={[talkKeyLabel(form.talkKey ?? "ControlRight"), tr(lang, "mysz", "mouse")]} pl="+ przeciągnij → Circle-to-Ask" en="+ drag → Circle-to-Ask" />
            </div>
            <Row
              label={tr(lang, "Słowo budzące", "Wake word")}
              hint={tr(lang, "Zdania zaczynające się od niego idą do poleceń.", "Sentences starting with it are routed to commands.")}
            >
              <input
                className="soul-input w-[180px] px-3 py-2 text-[13px]"
                value={form.wakeWord ?? ""}
                onChange={(e) => patch({ wakeWord: e.target.value })}
                placeholder="Soul"
              />
            </Row>
            <Row
              label={tr(lang, "Klawisz + przeciągnięcie = Circle-to-Ask", "Talk key + drag = Circle-to-Ask")}
              hint={tr(lang, "Przytrzymaj klawisz i zaznacz myszą fragment ekranu.", "Hold the key and drag a rectangle on screen.")}
            >
              <Toggle checked={form.talkDragAsk ?? true} onChange={(talkDragAsk) => patch({ talkDragAsk })} />
            </Row>
            <Row
              label={tr(lang, "Blokuj normalne działanie klawisza przy krótkim stuknięciu", "Block the key's normal action on short taps")}
              hint={tr(
                lang,
                "Tylko dla klawiszy przechwytywanych przez Soul (boczne przyciski, F-klawisze, Caps Lock…). Modyfikatory jak prawy Ctrl nigdy nie są blokowane.",
                "Only for keys Soul intercepts (side buttons, F-keys, Caps Lock…). Modifiers like Right Ctrl are never blocked.",
              )}
            >
              <Toggle checked={form.talkKeySwallow ?? false} onChange={(talkKeySwallow) => patch({ talkKeySwallow })} />
            </Row>
          </>
        ) : (
          <>
            <Row stacked label={tr(lang, "Dyktowanie (przytrzymaj)", "Dictation (hold)")}>
              <HotkeyRecorder mode="combo" value={form.hotkey} onChange={(hotkey) => patch({ hotkey })} placeholder="Ctrl+Super+Space" />
            </Row>
            {altgr(form.hotkey)}
            <Row stacked label={tr(lang, "Polecenia głosowe (przytrzymaj)", "Voice commands (hold)")}>
              <HotkeyRecorder mode="combo" value={form.agentHotkey} onChange={(agentHotkey) => patch({ agentHotkey })} placeholder="Super+Alt+Space" />
            </Row>
            {altgr(form.agentHotkey)}
          </>
        )}
      </Card>

      <Card
        title="Circle-to-Ask"
        description={tr(lang, "Zrzut ekranu → zaznacz → zapytaj głosem lub tekstem.", "Capture → select → ask by voice or text.")}
      >
        <HotkeyRecorder mode="combo" value={form.askHotkey} onChange={(askHotkey) => patch({ askHotkey })} placeholder="Ctrl+Shift+Space" />
        {altgr(form.askHotkey)}
      </Card>

      <Callout>
        <b className="font-semibold">AltGr:</b>{" "}
        {tr(
          lang,
          "Na polskim układzie prawy Alt to AltGr (= Ctrl+Alt). Soul nie pozwala na Ctrl+Alt+litera i ignoruje skróty, gdy AltGr jest wciśnięty — „ą, ć, ę” zawsze działają.",
          "On Polish layouts Right Alt is AltGr (= Ctrl+Alt). Soul refuses Ctrl+Alt+letter and ignores hotkeys while AltGr is held — “ą, ć, ę” always work.",
        )}
      </Callout>
    </>
  );
}

function Gesture({ keys, pl, en }: { keys: string[]; pl: string; en: string }) {
  const lang = useLang();
  return (
    <div className="rounded-2xl border border-white/[0.06] bg-white/[0.03] px-3 py-2.5">
      <div className="mb-1.5 flex flex-wrap items-center gap-1">
        {keys.map((k, i) => (
          <Keycap key={i}>{k}</Keycap>
        ))}
      </div>
      {lang === "en" ? en : pl}
    </div>
  );
}

// ───────────────────────────── Modele ─────────────────────────────
export function ModelsPage({ s }: P) {
  const lang = useLang();
  const { form, patch, models } = s;
  const retired = new Set(form.retiredModels ?? []);
  const live = models.filter((m) => !retired.has(m.id));
  const withSaved = (id: string | undefined) =>
    id && !live.some((m) => m.id === id) ? [{ id, displayName: `${id} (saved)` }, ...live] : live;
  const askId = form.askModel || form.model || DEFAULT_ASK;
  const opts = withSaved(form.model);
  const liveOff = form.useLiveApi === false;

  const dictOpts = opts
    .filter((m) => isTranscribeLive(m) || m.id === form.dictationModel)
    .concat(opts.some(isTranscribeLive) ? [] : [{ id: DEFAULT_DICTATION, displayName: "Gemini 3.5 Transcribe Live" }]);
  const cmdOpts = opts
    .filter((m) => isCommandsLive(m) || m.id === form.commandsModel)
    .concat(opts.some(isCommandsLive) ? [] : [{ id: DEFAULT_COMMANDS, displayName: "Gemini 3.8 Live" }]);

  return (
    <>
      {s.modelNotice && (
        <Callout tone="warn">
          <div className="flex items-start gap-3">
            <span className="min-w-0 flex-1">{s.modelNotice}</span>
            <button type="button" className="shrink-0 text-[11px] underline opacity-80 hover:opacity-100" onClick={s.dismissModelNotice}>
              {tr(lang, "Zamknij", "Dismiss")}
            </button>
          </div>
        </Callout>
      )}
      <Card
        title="Live API"
        description={tr(
          lang,
          "Strumieniuje dźwięk, gdy trzymasz klawisz — najszybsze wyniki. Wyłączone = tylko REST (wolniej, nagranie wysyłane po puszczeniu).",
          "Streams audio while you hold the key — fastest results. Off = REST only (slower, uploads after release).",
        )}
        action={<Toggle checked={form.useLiveApi ?? true} onChange={(useLiveApi) => patch({ useLiveApi })} />}
      >
        <Row label={tr(lang, "Dyktowanie (Live)", "Dictation (Live)")} disabled={liveOff}>
          <Select value={form.dictationModel || DEFAULT_DICTATION} onChange={(dictationModel) => patch({ dictationModel })} disabled={liveOff}>
            {dictOpts.map((m) => (
              <option key={`d-${m.id}`} value={m.id}>
                {m.displayName || m.id}
              </option>
            ))}
          </Select>
        </Row>
        <Row label={tr(lang, "Polecenia (Live)", "Commands (Live)")} disabled={liveOff}>
          <Select value={form.commandsModel || DEFAULT_COMMANDS} onChange={(commandsModel) => patch({ commandsModel })} disabled={liveOff}>
            {cmdOpts.map((m) => (
              <option key={`c-${m.id}`} value={m.id}>
                {m.displayName || m.id}
              </option>
            ))}
          </Select>
        </Row>
      </Card>
      <Card title="REST" description={s.loadingModels ? tr(lang, "Pobieram listę modeli…", "Loading models…") : undefined}>
        <Row label="Circle-to-Ask">
          <Select
            value={askId}
            onChange={(askModel) =>
              s.setForm((prev: SettingsData) => ({
                ...prev,
                askModel,
                model: askModel,
                fallbackModel:
                  prev.fallbackModel === askModel ? pickNewestFlashLite(askModel, models, prev.retiredModels) : prev.fallbackModel,
              }))
            }
          >
            {opts
              .filter((m) => isAskModel(m) || m.id === askId)
              .map((m) => (
                <option key={`a-${m.id}`} value={m.id}>
                  {m.displayName || m.id}
                </option>
              ))}
          </Select>
        </Row>
        <Row
          label={tr(lang, "Model zapasowy", "Fallback")}
          hint={tr(
            lang,
            "Dla Circle-to-Ask i gdy Live zawiedzie (503 / 429 / sieć). Dzienny limit 429 wyłącza model do północy czasu pacyficznego.",
            "For Circle-to-Ask and when Live fails (503 / 429 / network). A daily 429 marks a model exhausted until Pacific midnight.",
          )}
        >
          <Select value={form.fallbackModel || DEFAULT_ASK_FALLBACK} onChange={(fallbackModel) => patch({ fallbackModel })}>
            {withSaved(form.fallbackModel).map((m) => (
              <option key={`f-${m.id}`} value={m.id} disabled={m.id === askId}>
                {m.displayName || m.id}
                {m.id === askId ? tr(lang, " (to samo co Ask)", " (same as Ask)") : ""}
              </option>
            ))}
          </Select>
        </Row>
      </Card>
    </>
  );
}

// ───────────────────────────── Głos i dusza ─────────────────────────────
export function VoicePage({ s }: P) {
  const lang = useLang();
  const anim = useAnim();
  const { form, patch } = s;
  const voiceOn = form.voiceEnabled ?? true;
  return (
    <>
      <WakeWordCard />

      <Card
        title={tr(lang, "Mówione odpowiedzi", "Spoken replies")}
        action={<Toggle checked={voiceOn} onChange={(voiceEnabled) => patch({ voiceEnabled })} />}
        description={tr(lang, "Soul odpowiada głosem na polecenia i pytania. Naciśnij klawisz mówienia, by przerwać.", "Soul answers commands and questions out loud. Press the talk key to interrupt.")}
      >
        <Row
          label={tr(lang, "Tylko krótkie potwierdzenia", "Brief confirmations only")}
          hint={tr(lang, "Kilka słów („Otwieram Spotify”), nawet przy pytaniach.", "A few words (“Opening Spotify”), even for questions.")}
          disabled={!voiceOn}
        >
          <Toggle checked={form.voiceBrief ?? false} onChange={(voiceBrief) => patch({ voiceBrief })} />
        </Row>
        <Row
          label={tr(lang, "Głośność głosu Soul", "Soul voice volume")}
          hint={tr(lang, "Tylko głos Soul, nie głośność systemu. Głosem: „mów ciszej”, „mów głośniej”, „wycisz się”.", "Soul's voice only, not the system volume. By voice: “speak quieter / louder”, “mute yourself”.")}
          disabled={!voiceOn}
        >
          <Slider
            value={form.voiceVolume ?? 100}
            min={0}
            max={100}
            step={5}
            onChange={(v) => patch({ voiceVolume: Math.round(v) })}
            format={(v) => `${Math.round(v)}%`}
          />
        </Row>
        <Row
          label={tr(lang, "Ściszaj muzykę, gdy Soul mówi", "Duck music while Soul speaks")}
          hint={tr(lang, "Inne aplikacje grają ciszej (30%) na czas odpowiedzi, potem wracają.", "Other apps play quieter (30%) during a reply, then come back.")}
          disabled={!voiceOn}
        >
          <Toggle checked={form.duckMusic ?? false} onChange={(duckMusic) => patch({ duckMusic })} />
        </Row>
        <Row label={tr(lang, "Tempo mówienia", "Speaking speed")} disabled={!voiceOn}>
          <Segmented
            value={form.voiceSpeed ?? "normal"}
            options={[
              { value: "slow", label: tr(lang, "Wolno", "Slow") },
              { value: "normal", label: tr(lang, "Normalnie", "Normal") },
              { value: "fast", label: tr(lang, "Szybko", "Fast") },
            ]}
            onChange={(voiceSpeed) => patch({ voiceSpeed })}
          />
        </Row>
      </Card>

      <Card
        title={tr(lang, "Głos", "Voice")}
        description={
          form.apiKey
            ? tr(lang, "▶ odtwarza próbkę z bieżącą osobowością i tempem.", "▶ plays a sample with the current personality and speed.")
            : tr(lang, "Dodaj klucz API, aby odsłuchać próbki.", "Add an API key to preview voices.")
        }
      >
        <div className={`grid grid-cols-2 gap-2 ${voiceOn ? "" : "pointer-events-none opacity-45"}`}>
          {VOICES.map((v) => {
            const active = (form.voiceName ?? "Charon") === v.id;
            const playing = s.previewing === v.id;
            return (
              <div
                key={v.id}
                role="radio"
                aria-checked={active}
                tabIndex={0}
                onClick={() => patch({ voiceName: v.id })}
                onKeyDown={(e) => e.key === "Enter" && patch({ voiceName: v.id })}
                className={`flex cursor-pointer items-center gap-3 rounded-2xl border px-3 py-2.5 transition-colors ${
                  active ? "border-[var(--soul-accent)]/50 bg-[var(--soul-accent)]/[0.08]" : "border-white/[0.07] hover:bg-white/[0.04]"
                }`}
              >
                <motion.button
                  type="button"
                  aria-label={`${tr(lang, "Odsłuchaj", "Preview")} ${v.id}`}
                  disabled={!form.apiKey || s.previewing !== null}
                  onClick={(e) => {
                    e.stopPropagation();
                    void s.previewVoice(v.id);
                  }}
                  whileTap={anim.on("micro") ? { scale: 0.9 } : undefined}
                  className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/10 text-[11px] text-white disabled:opacity-40"
                  style={playing ? { background: "var(--soul-accent)", color: "#0b0d0c" } : undefined}
                >
                  {playing ? "■" : "▶"}
                </motion.button>
                <div className="min-w-0">
                  <div className="text-[13px] font-medium">{v.id}</div>
                  <div className="truncate text-[11px] text-[var(--soul-muted)]">{lang === "en" ? v.en : v.pl}</div>
                </div>
              </div>
            );
          })}
        </div>
      </Card>

      <Card
        title={tr(lang, "Wyszukiwanie", "Search")}
        description={tr(
          lang,
          "Pytania („jak…”, „co to…”) dostają krótką odpowiedź głosem i na wyspie (Gemini + Google Search).",
          "Questions (“how…”, “what is…”) get a short spoken + Island answer (Gemini + Google Search).",
        )}
      >
        <Row
          label={tr(lang, "Otwieraj przeglądarkę przy wyszukiwaniu", "Open the browser when searching")}
          hint={tr(lang, "Zwykłe wyszukiwania zawsze otwierają przeglądarkę.", "Plain (non-question) searches always open the browser.")}
        >
          <Toggle checked={form.searchOpenBrowser ?? true} onChange={(searchOpenBrowser) => patch({ searchOpenBrowser })} />
        </Row>
      </Card>

      <Card
        title={tr(lang, "Osobowość", "Personality")}
        description={tr(lang, "Zmienia tylko ton mówionych odpowiedzi. Dyktowanie zawsze zostaje neutralne.", "Changes the tone of spoken replies only. Dictation always stays neutral.")}
      >
        <div className="grid grid-cols-2 gap-2">
          {PERSONALITIES.map((p) => {
            const active = (form.personality ?? "neutral") === p.id;
            return (
              <button
                key={p.id}
                type="button"
                onClick={() => patch({ personality: p.id })}
                className={`relative rounded-2xl border px-3.5 py-3 text-left transition-colors ${
                  active ? "border-[var(--soul-accent)]/50 bg-[var(--soul-accent)]/[0.08]" : "border-white/[0.07] hover:bg-white/[0.04]"
                }`}
              >
                <div className="text-[13px] font-medium">{lang === "en" ? p.en : p.pl}</div>
                <div className="mt-0.5 text-[11.5px] italic text-[var(--soul-muted)]">{p.sample}</div>
              </button>
            );
          })}
        </div>
      </Card>
    </>
  );
}

// ───────────────────────────── Wyspa ─────────────────────────────
export function IslandPage({ s }: P) {
  const lang = useLang();
  const anim = useAnim();
  const { form, patch } = s;
  const on = form.islandEnabled ?? true;
  const sections: [keyof IslandSections, string, string][] = [
    ["nowPlaying", "Odtwarzanie", "Now playing"],
    ["clipboard", "Schowek", "Clipboard"],
    ["plans", "Plany", "Plans"],
    ["quickApps", "Aplikacje", "Quick apps"],
    ["quickSettings", "Szybkie ustawienia", "Quick settings"],
  ];
  return (
    <>
      <Card
        title={tr(lang, "Dynamiczna Wyspa", "Dynamic Island")}
        description={tr(lang, "Pigułka u góry ekranu: dusza, muzyka, schowek i stan asystenta.", "The pill at the top of the screen: spirit, music, clipboard and assistant state.")}
        action={<Toggle checked={on} onChange={(islandEnabled) => patch({ islandEnabled })} />}
      >
        <Row label={tr(lang, "Położenie", "Position")} disabled={!on}>
          <div className="flex gap-2">
            {(["topLeft", "topCenter", "topRight"] as const).map((pos) => {
              const active = (form.islandPosition ?? "topCenter") === pos;
              return (
                <button
                  key={pos}
                  type="button"
                  onClick={() => patch({ islandPosition: pos })}
                  aria-label={pos}
                  className={`relative h-[46px] w-[72px] rounded-lg border transition-colors ${
                    active ? "border-[var(--soul-accent)]/60 bg-[var(--soul-accent)]/[0.07]" : "border-white/10 bg-white/[0.03] hover:bg-white/[0.06]"
                  }`}
                >
                  <motion.span
                    layout={anim.on("micro")}
                    className="absolute top-1.5 h-[6px] w-[24px] rounded-full"
                    style={{
                      background: active ? "var(--soul-accent)" : "rgba(255,255,255,0.35)",
                      left: pos === "topLeft" ? 6 : pos === "topRight" ? 40 : 23,
                    }}
                  />
                </button>
              );
            })}
          </div>
        </Row>
      </Card>
      <Card title={tr(lang, "Karty w rozwiniętej wyspie", "Tabs in the expanded Island")}>
        <div className="grid grid-cols-2 gap-x-6 gap-y-3">
          {sections.map(([key, pl, en]) => (
            <Row key={key} label={lang === "en" ? en : pl} disabled={!on}>
              <Toggle
                checked={form.islandSections?.[key] ?? true}
                onChange={(v) =>
                  patch({
                    islandSections: {
                      nowPlaying: true,
                      clipboard: true,
                      plans: true,
                      quickApps: true,
                      quickSettings: true,
                      ...form.islandSections,
                      [key]: v,
                    },
                  })
                }
              />
            </Row>
          ))}
        </div>
      </Card>
      <Callout>
        {tr(
          lang,
          "Rozwijanie wyspy dla każdego trybu, tryb cichy i czas wyświetlania wyników: Wygląd → Wydajność i hałas.",
          "Per-mode Island expansion, quiet mode and result duration: Appearance → Performance & noise.",
        )}
      </Callout>
    </>
  );
}

// ───────────────────────────── Zaawansowane ─────────────────────────────
type SampleRoute = "none" | "command" | "dictation";

/** Dev only: record a voice regression sample into tests/voice/ (see voice_regression.rs). */
function VoiceSampleCard() {
  const lang = useLang();
  const [name, setName] = useState("");
  const [route, setRoute] = useState<SampleRoute>("command");
  const [device, setDevice] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState(0);
  const [, setTick] = useState(0);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const recording = device !== null;

  useEffect(() => {
    if (!recording) return;
    const id = window.setInterval(() => setTick((t) => t + 1), 250);
    return () => window.clearInterval(id);
  }, [recording]);

  // Leaving the page mid-take must not leave the mic open.
  const recordingRef = useRef(false);
  recordingRef.current = recording;
  useEffect(() => () => void (recordingRef.current && invoke("voice_sample_cancel")), []);

  const start = async () => {
    setBusy(true);
    setStatus(null);
    try {
      setDevice(await invoke<string>("voice_sample_start"));
      setStartedAt(Date.now());
    } catch (e) {
      setStatus({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };
  const stop = async () => {
    setBusy(true);
    try {
      const path = await invoke<string>("voice_sample_stop", {
        name,
        route: route === "none" ? null : route,
      });
      setDevice(null);
      setStatus({ ok: true, text: tr(lang, `Zapisano: ${path}`, `Saved: ${path}`) });
      setName("");
    } catch (e) {
      // Invalid name: still recording, fix the name and press Stop again.
      setStatus({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };
  const cancel = async () => {
    await invoke("voice_sample_cancel");
    setDevice(null);
  };

  const seconds = recording ? ((Date.now() - startedAt) / 1000).toFixed(1) : null;
  return (
    <Card
      title={tr(lang, "Próbki testów głosowych (dev)", "Voice test samples (dev)")}
      description={tr(
        lang,
        "Nagrywa z wybranego mikrofonu do tests/voice/ i dopisuje przypadek do cases.json. Uruchom: npm run test:voice.",
        "Records from the selected microphone into tests/voice/ and adds a case to cases.json. Run: npm run test:voice.",
      )}
    >
      <Row
        label={tr(lang, "Nazwa pliku", "File name")}
        hint={tr(lang, "np. nastepna albo dyktowanie_faktura", "e.g. nastepna or dyktowanie_faktura")}
      >
        <input
          className="soul-input w-[220px] px-3 py-2 text-[13px]"
          value={name}
          spellCheck={false}
          onChange={(e) => setName(e.target.value)}
          placeholder="nastepna"
        />
      </Row>
      <Row label={tr(lang, "Oczekiwana ścieżka", "Expected route")}>
        <Segmented<SampleRoute>
          size="sm"
          value={route}
          onChange={setRoute}
          options={[
            { value: "command", label: tr(lang, "Komenda", "Command") },
            { value: "dictation", label: tr(lang, "Dyktowanie", "Dictation") },
            { value: "none", label: tr(lang, "Brak", "None") },
          ]}
        />
      </Row>
      <Row
        label={
          recording
            ? tr(lang, `Nagrywanie… ${seconds} s`, `Recording… ${seconds} s`)
            : tr(lang, "Nagraj próbkę testową", "Record test sample")
        }
        hint={
          recording
            ? device
            : tr(lang, "Nie używaj klawisza mowy podczas nagrywania.", "Don't use the talk key while recording.")
        }
      >
        <div className="flex gap-2">
          {recording && (
            <Button onClick={cancel} disabled={busy}>
              {tr(lang, "Anuluj", "Cancel")}
            </Button>
          )}
          <Button
            variant={recording ? "danger" : "primary"}
            onClick={recording ? stop : start}
            disabled={busy || (recording && name.trim() === "")}
          >
            {recording ? tr(lang, "Zatrzymaj i zapisz", "Stop & save") : tr(lang, "Nagraj", "Record")}
          </Button>
        </div>
      </Row>
      {status && (
        <p
          className={`select-text break-all text-[12px] ${status.ok ? "text-[var(--soul-muted)]" : "text-[var(--soul-danger)]"}`}
        >
          {status.text}
        </p>
      )}
    </Card>
  );
}

export function AdvancedPage({ s }: P) {
  const lang = useLang();
  const { anim, update } = useAppearance();
  return (
    <>
      {import.meta.env.DEV && <VoiceSampleCard />}
      <Card
        title={tr(lang, "Dziennik błędów", "Error log")}
        action={
          <Button onClick={s.clearErrorLog} disabled={s.errorLog.length === 0}>
            {tr(lang, "Wyczyść", "Clear")}
          </Button>
        }
      >
        {s.errorLog.length === 0 ? (
          <p className="text-[12px] text-[var(--soul-muted)]">{tr(lang, "Brak błędów. 🌿", "No errors yet. 🌿")}</p>
        ) : (
          <ul className="soul-scroll max-h-56 space-y-2 overflow-y-auto rounded-xl border border-white/10 bg-black/30 p-2.5">
            {s.errorLog.map((entry, i) => (
              <li
                key={`${i}-${entry.slice(0, 24)}`}
                className="select-text whitespace-pre-wrap break-words font-mono text-[11px] leading-snug text-[var(--soul-danger)]"
              >
                {entry}
              </li>
            ))}
          </ul>
        )}
      </Card>
      <Card title={tr(lang, "Przywracanie", "Reset")}>
        <Row
          label={tr(lang, "Przywróć domyślny wygląd i animacje", "Reset appearance & animations")}
          hint={tr(lang, "Kolor, dusza, wyspa, animacje, wydajność i hałas.", "Accent, spirit, Island, animations, performance and noise.")}
        >
          <Button variant="danger" onClick={() => update({ ...DEFAULT_APPEARANCE, uiLang: anim.cfg.uiLang })}>
            {tr(lang, "Przywróć wszystko", "Reset all")}
          </Button>
        </Row>
      </Card>
      <Callout>
        {tr(lang, "Szczegółowe logi wyspy i mediów: uruchom Soul z", "Verbose Island / media logs: start Soul with")}{" "}
        <code className="rounded bg-white/10 px-1.5 py-0.5 font-mono text-[11px]">SOUL_DEBUG=1</code>.
      </Callout>
    </>
  );
}
