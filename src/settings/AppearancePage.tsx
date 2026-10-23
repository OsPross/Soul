import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import { useAppearance } from "../anim/AnimationProvider";
import {
  DEFAULT_APPEARANCE,
  FEATURES,
  presetAllows,
  type AppearanceConfig,
  type Feature,
  type Preset,
  type SpiritStyle,
} from "../anim/config";
import { tr } from "../i18n";
import { playCue } from "../island/sounds";
import { EqBars, Marquee, Waveform } from "../island/bits";
import Spirit, { SpiritOrDot, type SpiritState } from "../spirit/Spirit";
import { tokens } from "../theme/tokens";
import type { SettingsCtx } from "./Settings";
import { Button, Callout, Card, Row, SectionReset, Segmented, Slider, Toggle } from "./ui";

const SWATCHES = ["#7cf2c8", "#8fb8ff", "#c4a1ff", "#ff9ec7", "#ffb547", "#9ef07c", "#7ce3f2", "#f5f5f7"];

const FEATURE_TEXT: Record<Feature, [string, string, string, string]> = {
  spirit: ["Dusza", "Spirit", "Wyłączona = prosta kropka statusu", "Off = simple status dot"],
  spiritIdle: ["Życie w spoczynku", "Idle life", "Oddech, mruganie, unoszenie, rozglądanie", "Breathing, blinking, floating, glancing"],
  particles: ["Cząsteczki i iskry", "Particles & sparkles", "Wir przy myśleniu, iskry przy sukcesie", "Thinking swirl, success sparkles"],
  spiritMic: ["Reakcja na mikrofon", "Reacts to the mic", "Blask pulsuje z głosem", "Glow pulses with your voice"],
  musicSway: ["Kołysanie do muzyki", "Music sway", "Dusza tańczy, gdy coś gra", "Spirit sways while music plays"],
  morph: ["Sprężyste morfowanie wyspy", "Island morph springs", "Wyłączone = natychmiastowa zmiana", "Off = instant resize"],
  tabs: ["Przejścia kart", "Tab transitions", "Przesuwany wskaźnik i przenikanie", "Sliding indicator and crossfade"],
  micro: ["Mikrointerakcje", "Micro-interactions", "Uniesienie przy najechaniu, wciśnięcie", "Hover lift, press scale"],
  marquee: ["Przewijany tytuł", "Title marquee", "Wyłączony = wielokropek", "Off = ellipsis"],
  equalizer: ["Equalizer", "Equalizer bars", "Słupki przy odtwarzaniu", "Bars while playing"],
  waveform: ["Fala dźwięku", "Waveform", "Pod duszą przy słuchaniu i mówieniu", "While listening and speaking"],
  albumTint: ["Kolor z okładki", "Album-art tint", "Akcent z okładki albumu", "Accent from the album cover"],
  glassBlur: ["Rozmycie szkła", "Glass blur", "Rozmycie tła za wyspą", "Backdrop blur behind the Island"],
  effects: ["Efekty sukcesu i błędu", "Success / error effects", "Podskok, potrząśnięcie", "Bounce, shake"],
  askEffects: ["Efekty zaznaczania", "Selection effects", "Pierścień i poświata Circle-to-Ask", "Circle-to-Ask ring and glow"],
  caption: ["Pasek z tekstem po dyktowaniu", "Caption after dictation", "Pokazuje wklejony tekst", "Shows the pasted text"],
};

const PREVIEW_STATES: SpiritState[] = ["idle", "listening", "thinking", "speaking", "success", "error", "music"];
const STATE_TEXT: Record<SpiritState, [string, string]> = {
  idle: ["Spokój", "Idle"],
  listening: ["Słucha", "Listening"],
  thinking: ["Myśli", "Thinking"],
  speaking: ["Mówi", "Speaking"],
  success: ["Sukces", "Success"],
  error: ["Błąd", "Error"],
  music: ["Muzyka", "Music"],
};

function previewColor(st: SpiritState, accent: string): string {
  if (st === "listening") return tokens.accentDictation;
  if (st === "thinking" || st === "speaking") return tokens.accentCommand;
  if (st === "success") return tokens.success;
  if (st === "error") return tokens.danger;
  return accent;
}

export default function AppearancePage(_: { s: SettingsCtx }) {
  const { anim, update } = useAppearance();
  const cfg = anim.cfg;
  const lang = cfg.uiLang;
  const [tab, setTab] = useState<"style" | "anim" | "perf">("style");
  const [demo, setDemo] = useState<SpiritState>("idle");
  const [auto, setAuto] = useState(true);

  // Auto-cycle the preview through every state.
  useEffect(() => {
    if (!auto) return;
    const id = window.setInterval(() => {
      setDemo((d) => PREVIEW_STATES[(PREVIEW_STATES.indexOf(d) + 1) % PREVIEW_STATES.length]);
    }, 2400);
    return () => window.clearInterval(id);
  }, [auto]);

  const set = (next: Partial<AppearanceConfig>) => update({ ...cfg, ...next });
  const setAnim = (next: Partial<AppearanceConfig["anim"]>) => update({ ...cfg, anim: { ...cfg.anim, ...next } });
  const setFeature = (f: Feature, v: boolean) => setAnim({ features: { ...cfg.anim.features, [f]: v } });
  const setPerf = (next: Partial<AppearanceConfig["perf"]>) => update({ ...cfg, perf: { ...cfg.perf, ...next } });
  const setNoise = (next: Partial<AppearanceConfig["noise"]>) => update({ ...cfg, noise: { ...cfg.noise, ...next } });
  const D = DEFAULT_APPEARANCE;
  const reset = tr(lang, "Domyślne", "Defaults");

  const limited =
    anim.limitedBy === "reduceMotion"
      ? tr(lang, "System prosi o ograniczenie ruchu — działa preset Minimalne.", "Your OS asks to reduce motion — the Minimal preset is active.")
      : anim.limitedBy === "battery"
        ? tr(lang, "Oszczędzanie baterii: zasilanie bateryjne lub aplikacja pełnoekranowa — działa preset Minimalne.", "Battery saver: on battery or a fullscreen app — the Minimal preset is active.")
        : null;

  return (
    <>
      {/* Live preview */}
      <Card className="overflow-hidden">
        <div className="flex items-center gap-6">
          <div
            className="flex h-[132px] w-[150px] shrink-0 items-center justify-center rounded-2xl"
            style={{ background: "radial-gradient(circle at 50% 45%, rgba(255,255,255,0.05), transparent 70%)" }}
          >
            <SpiritOrDot state={demo} color={previewColor(demo, cfg.accent)} size={62} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="mb-2 text-[12px] text-[var(--soul-muted)]">{tr(lang, "Podgląd na żywo", "Live preview")}</div>
            <div className="mb-3 flex flex-wrap gap-1.5">
              {PREVIEW_STATES.map((st) => (
                <button
                  key={st}
                  type="button"
                  onClick={() => {
                    setAuto(false);
                    setDemo(st);
                  }}
                  className={`rounded-full px-2.5 py-1 text-[11px] transition-colors ${
                    demo === st ? "bg-white/15 text-white" : "bg-white/[0.04] text-white/50 hover:text-white/80"
                  }`}
                >
                  {STATE_TEXT[st][lang === "en" ? 1 : 0]}
                </button>
              ))}
              <button
                type="button"
                onClick={() => setAuto((a) => !a)}
                className="rounded-full px-2.5 py-1 text-[11px] text-white/40 hover:text-white/80"
              >
                {auto ? "⏸" : "▶"} {tr(lang, "auto", "auto")}
              </button>
            </div>
            {/* Mini Island mock */}
            <motion.div
              layout
              transition={anim.spring("morph")}
              className="island-glass flex h-10 items-center gap-2 rounded-full px-2.5"
              style={{ width: demo === "music" ? 300 : demo === "idle" ? 210 : 280, ["--accent" as string]: previewColor(demo, cfg.accent) }}
            >
              <SpiritOrDot state={demo} color={previewColor(demo, cfg.accent)} size={16} />
              {demo === "music" ? (
                <>
                  <Marquee text="Soul — Spirit Sessions (Live at the Island)" className="flex-1 text-[12px] font-medium" />
                  <EqBars playing color={cfg.accent} />
                </>
              ) : (
                <span className="flex-1 truncate text-[12px] font-medium text-white/85">
                  {demo === "idle" ? "12:34" : STATE_TEXT[demo][lang === "en" ? 1 : 0]}
                </span>
              )}
            </motion.div>
          </div>
        </div>
      </Card>

      <div className="flex justify-center">
        <Segmented
          value={tab}
          onChange={setTab}
          options={[
            { value: "style", label: tr(lang, "Styl", "Style") },
            { value: "anim", label: tr(lang, "Animacje", "Animations") },
            { value: "perf", label: tr(lang, "Wydajność i hałas", "Performance & noise") },
          ]}
        />
      </div>

      {tab === "style" && (
        <Card
          title={tr(lang, "Styl", "Style")}
          action={
            <SectionReset
              label={reset}
              onClick={() =>
                set({ spiritStyle: D.spiritStyle, accent: D.accent, spiritSize: D.spiritSize, islandOpacity: D.islandOpacity, blur: D.blur, glow: D.glow })
              }
            />
          }
        >
          <Row label={tr(lang, "Styl duszy", "Spirit style")}>
            <Segmented<SpiritStyle>
              value={cfg.spiritStyle}
              onChange={(spiritStyle) => set({ spiritStyle })}
              options={[
                { value: "droplet", label: tr(lang, "Duszek (SVG)", "Droplet (SVG)") },
                { value: "orb", label: tr(lang, "Kula 3D (WebGL)", "3D Orb (WebGL)") },
              ]}
            />
          </Row>
          <Row label={tr(lang, "Kolor akcentu", "Accent colour")} hint={tr(lang, "Blask duszy i kontrolki. Okładka albumu może go nadpisać.", "Spirit glow and controls. Album art can override it.")}>
            <div className="flex items-center gap-1.5">
              {SWATCHES.map((c) => (
                <button
                  key={c}
                  type="button"
                  aria-label={c}
                  onClick={() => set({ accent: c })}
                  className="h-6 w-6 rounded-full transition-transform hover:scale-110"
                  style={{
                    background: c,
                    boxShadow: cfg.accent.toLowerCase() === c ? `0 0 0 2px #0e0e11, 0 0 0 4px ${c}` : "none",
                  }}
                />
              ))}
              <label className="relative ml-1 h-6 w-6 cursor-pointer overflow-hidden rounded-full border border-white/20" title={tr(lang, "Własny", "Custom")}>
                <span className="absolute inset-0" style={{ background: "conic-gradient(red, yellow, lime, cyan, blue, magenta, red)" }} />
                <input type="color" value={cfg.accent} onChange={(e) => set({ accent: e.target.value })} className="absolute inset-0 cursor-pointer opacity-0" />
              </label>
            </div>
          </Row>
          <Row label={tr(lang, "Rozmiar duszy", "Spirit size")}>
            <Slider value={cfg.spiritSize} min={0.8} max={1.4} onChange={(spiritSize) => set({ spiritSize })} format={(v) => `${Math.round(v * 100)}%`} />
          </Row>
          <Row label={tr(lang, "Intensywność blasku", "Glow intensity")}>
            <Slider value={cfg.glow} min={0} max={1.5} onChange={(glow) => set({ glow })} format={(v) => `${Math.round(v * 100)}%`} />
          </Row>
          <Row label={tr(lang, "Krycie wyspy", "Island opacity")}>
            <Slider value={cfg.islandOpacity} min={0.8} max={1} onChange={(islandOpacity) => set({ islandOpacity })} format={(v) => `${Math.round(v * 100)}%`} />
          </Row>
          <Row label={tr(lang, "Siła rozmycia", "Blur strength")} disabled={!anim.on("glassBlur")}>
            <Slider value={cfg.blur} min={0} max={40} step={1} onChange={(blur) => set({ blur })} format={(v) => `${v}px`} />
          </Row>
        </Card>
      )}

      {tab === "anim" && (
        <>
          <Card
            title={tr(lang, "Animacje", "Animations")}
            description={tr(lang, "Zmiany działają od razu, bez restartu.", "Changes apply instantly, no restart.")}
            action={
              <SectionReset
                label={reset}
                onClick={() => setAnim({ ...D.anim })}
              />
            }
          >
            <Row label={tr(lang, "Preset", "Preset")}>
              <Segmented<Preset>
                value={cfg.anim.preset}
                onChange={(preset) => setAnim({ preset })}
                options={[
                  { value: "full", label: tr(lang, "Pełne", "Full") },
                  { value: "balanced", label: tr(lang, "Zrównoważone", "Balanced") },
                  { value: "minimal", label: tr(lang, "Minimalne", "Minimal") },
                  { value: "off", label: tr(lang, "Wyłączone", "Off") },
                ]}
              />
            </Row>
            <Row
              label={tr(lang, "Słuchaj systemowego „ogranicz ruch”", "Follow OS reduce motion")}
              hint={tr(lang, "Gdy system o to prosi, wymusza Minimalne.", "Forces Minimal when your OS asks for less motion.")}
            >
              <Toggle checked={cfg.anim.followReduceMotion} onChange={(followReduceMotion) => setAnim({ followReduceMotion })} />
            </Row>
            {limited && <Callout tone="warn">{limited}</Callout>}
            <Row label={tr(lang, "Szybkość animacji", "Animation speed")}>
              <Slider value={cfg.anim.speed} min={0.5} max={2} onChange={(speed) => setAnim({ speed })} format={(v) => `${v.toFixed(2)}×`} />
            </Row>
            <Row label={tr(lang, "Sprężystość", "Spring stiffness")}>
              <Slider
                value={cfg.anim.stiffness}
                min={0}
                max={1}
                onChange={(stiffness) => setAnim({ stiffness })}
                format={(v) => `${Math.round(v * 100)}%`}
                leftLabel={tr(lang, "miękko", "soft")}
                rightLabel={tr(lang, "żwawo", "snappy")}
              />
            </Row>
          </Card>
          <Card
            title={tr(lang, "Funkcje", "Features")}
            description={tr(lang, "Każda niezależnie. Przyciemnione = wyłączone przez bieżący preset.", "Each one independent. Dimmed = disabled by the current preset.")}
          >
            <div className="grid grid-cols-2 gap-x-6 gap-y-3.5">
              {FEATURES.map((f) => {
                const t = FEATURE_TEXT[f];
                const blocked = !presetAllows(anim.level, f);
                return (
                  <div key={f} className={`flex items-center justify-between gap-3 transition-opacity ${blocked ? "opacity-45" : ""}`}>
                    <div className="min-w-0">
                      <div className="text-[12.5px] text-white/90">
                        {lang === "en" ? t[1] : t[0]}
                        {blocked && <span className="ml-1.5 rounded bg-white/10 px-1 py-px text-[9.5px] text-white/50">preset</span>}
                      </div>
                      <div className="truncate text-[11px] text-[var(--soul-muted)]">{lang === "en" ? t[3] : t[2]}</div>
                    </div>
                    <Toggle checked={cfg.anim.features[f]} onChange={(v) => setFeature(f, v)} label={t[1]} />
                  </div>
                );
              })}
            </div>
          </Card>
          {anim.on("waveform") && (
            <Card title={tr(lang, "Podgląd fali", "Waveform preview")} description={tr(lang, "Reaguje na mikrofon, gdy Soul słucha.", "Reacts to the mic while Soul listens.")}>
              <div className="flex justify-center">
                <Waveform source="mic" color={cfg.accent} bars={28} height={28} />
              </div>
            </Card>
          )}
        </>
      )}

      {tab === "perf" && (
        <>
          <Card
            title={tr(lang, "Wydajność", "Performance")}
            action={<SectionReset label={reset} onClick={() => setPerf({ ...D.perf })} />}
          >
            <Row
              label={tr(lang, "Oszczędzanie baterii", "Battery saver")}
              hint={tr(lang, "Automatycznie Minimalne na baterii, w grach i aplikacjach pełnoekranowych.", "Auto Minimal on battery, in games and fullscreen apps.")}
            >
              <Toggle checked={cfg.perf.batterySaver} onChange={(batterySaver) => setPerf({ batterySaver })} />
            </Row>
            <Row label={tr(lang, "Limit klatek", "FPS cap")}>
              <Segmented<number>
                value={cfg.perf.fpsCap}
                onChange={(v) => setPerf({ fpsCap: v === 30 ? 30 : 60 })}
                options={[
                  { value: 30, label: "30" },
                  { value: 60, label: "60" },
                ]}
              />
            </Row>
            <Row
              label={tr(lang, "Usypiaj animacje w spoczynku", "Pause animations when idle")}
              hint={tr(lang, "Gdy wyspa jest zwinięta i nic się nie dzieje przez 30 s.", "When the Island is collapsed and idle for 30 s.")}
            >
              <Toggle checked={cfg.perf.pauseWhenIdle} onChange={(pauseWhenIdle) => setPerf({ pauseWhenIdle })} />
            </Row>
          </Card>
          <Card
            title={tr(lang, "Hałas", "Noise")}
            description={tr(lang, "Kiedy wyspa ma się rozwijać i jak długo pokazywać wyniki.", "When the Island should expand and how long results stay.")}
            action={<SectionReset label={reset} onClick={() => setNoise({ ...D.noise })} />}
          >
            <Row label={tr(lang, "Tryb cichy", "Quiet mode")} hint={tr(lang, "Wyspa zostaje kompaktowa, bez dźwięków.", "Island stays compact, no sounds.")}>
              <Toggle checked={cfg.noise.quiet} onChange={(quiet) => setNoise({ quiet })} />
            </Row>
            <Row label={tr(lang, "Rozwijaj przy dyktowaniu", "Expand for dictation")} disabled={cfg.noise.quiet}>
              <Toggle checked={cfg.noise.expandOnDictation} onChange={(expandOnDictation) => setNoise({ expandOnDictation })} />
            </Row>
            <Row label={tr(lang, "Rozwijaj przy poleceniach", "Expand for commands")} disabled={cfg.noise.quiet}>
              <Toggle checked={cfg.noise.expandOnCommand} onChange={(expandOnCommand) => setNoise({ expandOnCommand })} />
            </Row>
            <Row
              label={tr(lang, "Dusza-przewodnik w Circle-to-Ask", "Spirit guide in Circle-to-Ask")}
              hint={tr(lang, "Mała dusza podąża za kursorem i siada przy zaznaczeniu.", "A small spirit follows the cursor and docks at the selection.")}
            >
              <Toggle checked={cfg.noise.expandOnAsk} onChange={(expandOnAsk) => setNoise({ expandOnAsk })} />
            </Row>
            <Row label={tr(lang, "Czas wyświetlania wyniku", "Result display duration")}>
              <Slider
                value={cfg.noise.resultMs}
                min={800}
                max={8000}
                step={100}
                onChange={(resultMs) => setNoise({ resultMs })}
                format={(v) => `${(v / 1000).toFixed(1)} s`}
              />
            </Row>
            <Row
              label={tr(lang, "Efekty dźwiękowe", "Sound effects")}
              hint={tr(lang, "Syntezowane lokalnie: start i koniec słuchania, sukces, błąd, powiadomienie.", "Synthesized locally: listen start / end, success, error, notification.")}
            >
              <Toggle checked={cfg.noise.sounds} onChange={(sounds) => setNoise({ sounds })} />
            </Row>
            <Row label={tr(lang, "Zestaw dźwięków", "Sound pack")}>
              <Segmented<string>
                value={cfg.noise.sounds ? cfg.noise.soundPack : "off"}
                onChange={(v) => {
                  if (v === "off") setNoise({ sounds: false });
                  else {
                    const soundPack = v === "subtle" ? "subtle" : "futuristic";
                    setNoise({ sounds: true, soundPack });
                    playCue("listen", cfg.noise.soundVolume, soundPack);
                  }
                }}
                options={[
                  { value: "futuristic", label: tr(lang, "Futurystyczny", "Futuristic") },
                  { value: "subtle", label: tr(lang, "Subtelny", "Subtle") },
                  { value: "off", label: tr(lang, "Wyłączony", "Off") },
                ]}
              />
            </Row>
            <Row label={tr(lang, "Głośność efektów", "Effects volume")} disabled={!cfg.noise.sounds}>
              <Slider value={cfg.noise.soundVolume} min={0} max={1} onChange={(soundVolume) => setNoise({ soundVolume })} format={(v) => `${Math.round(v * 100)}%`} />
            </Row>
            <Row label={tr(lang, "Odsłuchaj", "Preview")} disabled={!cfg.noise.sounds} stacked>
              <div className="flex flex-wrap gap-1.5">
                {(
                  [
                    ["listen", "Start", "Start"],
                    ["listenEnd", "Koniec", "End"],
                    ["success", "Sukces", "Success"],
                    ["error", "Błąd", "Error"],
                    ["notify", "Powiadomienie", "Notification"],
                    ["timer", "Timer", "Timer"],
                  ] as const
                ).map(([cue, pl, en]) => (
                  <Button key={cue} onClick={() => playCue(cue, cfg.noise.soundVolume, cfg.noise.soundPack)}>
                    ▶ {tr(lang, pl, en)}
                  </Button>
                ))}
              </div>
            </Row>
          </Card>
        </>
      )}

      <div className="flex items-center justify-between px-1 pt-1">
        <span className="flex items-center gap-2 text-[11px] text-white/35">
          <Spirit state="idle" color={cfg.accent} size={11} />
          {tr(lang, "Zapisywane automatycznie", "Saved automatically")}
        </span>
        <Button variant="danger" onClick={() => update({ ...D, uiLang: cfg.uiLang })}>
          {tr(lang, "Przywróć wszystkie domyślne", "Reset everything")}
        </Button>
      </div>
    </>
  );
}
