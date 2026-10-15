import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, LayoutGroup, motion } from "framer-motion";
import { useIslandMode, type IslandMode } from "./useIslandMode";
import { reconcile, trackKeyOf, type ProgressAnchor } from "./progress";
import { useAnim, useT } from "../anim/AnimationProvider";
import { SpiritOrDot, type SpiritState } from "../spirit/Spirit";
import { accentFor, tokens, type ModeKind } from "../theme/tokens";
import { useAlbumAccent } from "./albumAccent";
import { EqBars, Marquee, Press, Waveform } from "./bits";
import {
  AppsPanel,
  Artwork,
  ClipboardPanel,
  NowPlayingPanel,
  PlansPanel,
  QuickSettingsPanel,
  Transport,
  type ClipboardItem,
  type NowPlaying,
  type PlanItem,
} from "./panels";
import { playCue, type Cue } from "./sounds";
import { tr } from "../i18n";
import { hasPendingUpdate, installUpdate, useUpdate, type UpdateStatus } from "../lib/update";

/** Transparent window margin around the panel; must match PAD_X / PAD_B in island/mod.rs. */
const PAD_X = 20;
const PAD_B = 28;

type AState =
  | "idle"
  | "listening"
  | "processing"
  | "thinking"
  | "retrying"
  | "confirm"
  | "speaking"
  | "done"
  | "error";

interface Banner {
  state: AState;
  message?: string | null;
  kind?: ModeKind | null;
}

interface IslandSections {
  nowPlaying: boolean;
  clipboard: boolean;
  plans: boolean;
  quickApps: boolean;
  quickSettings: boolean;
}

interface IslandSnapshot {
  position: string;
  sections: IslandSections;
  pinnedApps: string[];
  assistant: Banner | null;
  nowPlaying: NowPlaying;
  clipboard: ClipboardItem[];
  plans: PlanItem[];
  timers?: TimerItem[];
  clock: string;
}

export interface TimerItem {
  id: number;
  label: string;
  endsAtMs: number;
  totalMs: number;
}

interface ToastAction {
  label: string;
  command: string;
  args: Record<string, unknown>;
}

interface Toast {
  id: number;
  kind: "memory" | "note" | "timer" | "scene" | "wake" | "info" | "text";
  text: string;
  action?: ToastAction | null;
}

/** Backend commands a toast button may invoke (never anything else). */
const TOAST_COMMANDS = new Set(["memory_delete", "scene_create_suggested", "text_action_undo"]);
/** `text` = „Cofnij” after a replace text action: valid for 10 s (`text_action::UNDO_WINDOW`). */
const TOAST_MS: Record<Toast["kind"], number> = { memory: 7000, note: 6000, timer: 15000, scene: 15000, wake: 7000, info: 6000, text: 10000 };

/** Summary / explanation of the selected text (`island://text-result`). */
interface TextResult {
  id: number;
  title: string;
  text: string;
}
/** An unread result card goes away on its own after this long. */
const TEXT_RESULT_MS = 120_000;

/** "4:05", "1:02:03" */
export function countdown(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

interface ToolChip {
  id: number;
  name: string;
  label?: string | null;
  phase: "start" | "ok" | "error";
}

type Tab = "now" | "clipboard" | "plans" | "apps" | "settings";

/**
 * Collapsed-mode content. Priority: confirm > live activity > caption > music > idle.
 * `stage` = big live-activity card, `compact` = same info squeezed into the pill (quiet mode
 * or "don't expand" for that feature).
 */
type Stage = "confirm" | "result" | "toast" | "stage" | "compact" | "update" | "caption" | "music" | "idle";

const SIZE = {
  idle: { w: 236, h: 40, r: 20 },
  music: { w: 340, h: 40, r: 20 },
  compact: { w: 300, h: 40, r: 20 },
  stage: { w: 420, h: 84, r: 28 },
  stageLong: { w: 460, h: 118, r: 28 },
  confirm: { w: 400, h: 140, r: 28 },
  result: { w: 540, h: 230, r: 28 },
  toast: { w: 440, h: 76, r: 26 },
  update: { w: 420, h: 76, r: 26 },
  caption: { w: 440, h: 84, r: 28 },
  peekIdle: { w: 320, h: 72, r: 26 },
  peekMusic: { w: 430, h: 96, r: 26 },
  expanded: { w: 640, h: 340, r: 28 },
} as const;

const emptyNowPlaying = (): NowPlaying => ({
  title: "",
  artist: "",
  album: "",
  artworkDataUrl: "",
  playing: false,
  available: false,
  positionMs: 0,
  durationMs: 0,
  positionUpdatedAtMs: 0,
  playbackRate: 1,
});

const IDLE_PAUSE_MS = 30_000;
/** How long the collapsed "new version" notice stays (it stays in peek until installed). */
const UPDATE_NOTICE_MS = 12_000;

function spiritStateOf(b: Banner, music: boolean): SpiritState {
  switch (b.state) {
    case "listening":
    case "confirm":
      return "listening";
    case "processing":
    case "thinking":
    case "retrying":
      return "thinking";
    case "speaking":
      return "speaking";
    case "done":
      return "success";
    case "error":
      return "error";
    default:
      return music ? "music" : "idle";
  }
}

export default function Island() {
  const anim = useAnim();
  const t = useT();
  // Cards with buttons (text result, toast with an action) must not turn into peek on hover.
  const holdCard = useRef(false);
  const { mode, collapse, toggle, expand, handlers } = useIslandMode(holdCard);
  const cfg = anim.cfg;

  const [clock, setClock] = useState("--:--");
  const [banner, setBanner] = useState<Banner>({ state: "idle" });
  const [shown, setShown] = useState<Banner>({ state: "idle" });
  const [np, setNp] = useState<NowPlaying>(emptyNowPlaying);
  const [anchor, setAnchor] = useState<ProgressAnchor | null>(null);
  const [clipboard, setClipboard] = useState<ClipboardItem[]>([]);
  const [plans, setPlans] = useState<PlanItem[]>([]);
  const [pinned, setPinned] = useState<string[]>([]);
  const [sections, setSections] = useState<IslandSections>({
    nowPlaying: true,
    clipboard: true,
    plans: true,
    quickApps: true,
    quickSettings: true,
  });
  const [align, setAlign] = useState<"center" | "start" | "end">("center");
  const [tab, setTab] = useState<Tab>("now");
  const [caption, setCaption] = useState<string | null>(null);
  const [tools, setTools] = useState<ToolChip[]>([]);
  const [paused, setPaused] = useState(false);
  const [timers, setTimers] = useState<TimerItem[]>([]);
  const [toast, setToast] = useState<Toast | null>(null);
  const [textResult, setTextResult] = useState<TextResult | null>(null);
  /** Conversation context alive until this wall-clock time (ms), for the subtle dot. */
  const [contextUntil, setContextUntil] = useState<number | null>(null);
  const [wake, setWake] = useState<WakeMicState | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const shownAt = useRef(0);
  const update = useUpdate();
  const [updateNotice, setUpdateNotice] = useState<string | null>(null);
  const announced = useRef<string | null>(null);

  useEffect(() => {
    document.documentElement.classList.add("island-root");
    return () => document.documentElement.classList.remove("island-root");
  }, []);

  const applyNowPlaying = useCallback((next0: NowPlaying) => {
    const now = Date.now();
    const next: NowPlaying = {
      ...emptyNowPlaying(),
      ...next0,
      positionUpdatedAtMs: next0.positionUpdatedAtMs || now,
      playbackRate: next0.playbackRate && next0.playbackRate > 0 ? next0.playbackRate : 1,
    };
    // Periodic resyncs arrive without artwork; keep it while the track is unchanged.
    setNp((prev) => ({
      ...next,
      artworkDataUrl:
        next.artworkDataUrl || (trackKeyOf(prev) === trackKeyOf(next) ? prev.artworkDataUrl : ""),
    }));
    setAnchor((prev) => (next.available ? reconcile(prev, next, now) : null));
  }, []);

  // Results linger for `resultMs` even if Rust clears the banner sooner.
  const resultMs = cfg.noise.resultMs;
  useEffect(() => {
    if (banner.state === "idle" && (shown.state === "done" || shown.state === "error")) {
      const left = Math.max(0, resultMs - (Date.now() - shownAt.current));
      const id = window.setTimeout(() => setShown({ state: "idle" }), left);
      return () => window.clearTimeout(id);
    }
    if (banner !== shown) {
      shownAt.current = Date.now();
      setShown(banner);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [banner, resultMs]);

  // Caption strip after dictation.
  useEffect(() => {
    if (!caption) return;
    const id = window.setTimeout(() => setCaption(null), resultMs + 400);
    return () => window.clearTimeout(id);
  }, [caption, resultMs]);

  // Sound cues (synthesized locally; pack / volume / quiet mode from Appearance).
  const soundCfg = useRef(cfg.noise);
  soundCfg.current = cfg.noise;
  const cue = useCallback((c: Cue) => {
    const n = soundCfg.current;
    // The timer alarm is functional: it still rings (default pack) when effects are off.
    if (c === "timer" && (!n.sounds || n.quiet)) return playCue("timer", 0.5, "futuristic");
    if (!n.sounds || n.quiet) return;
    playCue(c, n.soundVolume, n.soundPack);
  }, []);
  const prevState = useRef<AState>("idle");
  useEffect(() => {
    const prev = prevState.current;
    prevState.current = banner.state;
    if (prev === banner.state) return;
    if (banner.state === "listening") cue("listen");
    else if (prev === "listening" && (banner.state === "processing" || banner.state === "thinking")) cue("listenEnd");
    else if (banner.state === "done") cue("success");
    else if (banner.state === "error") cue("error");
  }, [banner.state, cue]);

  // Toasts disappear on their own.
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast((t) => (t?.id === toast.id ? null : t)), TOAST_MS[toast.kind] ?? 6000);
    return () => window.clearTimeout(id);
  }, [toast]);

  useEffect(() => {
    if (!textResult) return;
    const id = window.setTimeout(() => setTextResult((r) => (r?.id === textResult.id ? null : r)), TEXT_RESULT_MS);
    return () => window.clearTimeout(id);
  }, [textResult]);

  useEffect(() => {
    if (contextUntil === null) return;
    const id = window.setTimeout(() => setContextUntil(null), Math.max(0, contextUntil - Date.now()));
    return () => window.clearTimeout(id);
  }, [contextUntil]);

  // Second ticks only while a countdown is visible.
  useEffect(() => {
    if (timers.length === 0) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    setNowMs(Date.now());
    return () => window.clearInterval(id);
  }, [timers.length]);

  // New listening turn → fresh tool chips; a finished turn settles any chip still spinning.
  useEffect(() => {
    if (shown.state === "listening") setTools([]);
    else if (shown.state === "done" || shown.state === "error" || shown.state === "idle") {
      const settled = shown.state === "error" ? "error" : "ok";
      setTools((prev) => (prev.some((c) => c.phase === "start") ? prev.map((c) => (c.phase === "start" ? { ...c, phase: settled } : c)) : prev));
    }
  }, [shown.state]);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    let alive = true;

    const loadSnapshot = (initial: boolean) =>
      invoke<IslandSnapshot>("island_get_snapshot")
        .then((snap) => {
          if (!alive) return;
          setAlign(snap.position === "topLeft" ? "start" : snap.position === "topRight" ? "end" : "center");
          if (!initial) return;
          setClock(snap.clock);
          setSections(snap.sections);
          setPinned(snap.pinnedApps);
          applyNowPlaying(snap.nowPlaying);
          setClipboard(snap.clipboard);
          setPlans(snap.plans);
          setTimers(snap.timers ?? []);
          if (snap.assistant) setBanner(snap.assistant);
          const first = (
            [
              snap.sections.nowPlaying && "now",
              snap.sections.clipboard && "clipboard",
              snap.sections.plans && "plans",
              snap.sections.quickApps && "apps",
              snap.sections.quickSettings && "settings",
            ] as const
          ).find(Boolean) as Tab | undefined;
          if (first) setTab(first);
        })
        .catch(() => undefined);
    void loadSnapshot(true);
    invoke<WakeMicState>("wake_status")
      .then((w) => alive && setWake(w))
      .catch(() => undefined);

    const tick = window.setInterval(() => {
      const d = new Date();
      setClock(`${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`);
    }, 1000);

    const on = <T,>(event: string, cb: (payload: T) => void) =>
      listen<T>(event, (e) => cb(e.payload)).then((u) => (alive ? unsubs.push(u) : u()));

    on<NowPlaying>("island://now-playing", applyNowPlaying);
    on<ClipboardItem[]>("island://clipboard", (items) => setClipboard(items ?? []));
    on<Banner>("island://assistant", (a) => setBanner({ ...a }));
    on<{ text: string }>("island://caption", (c) => setCaption(c.text));
    on<TimerItem[]>("island://timers", (list) => setTimers(list ?? []));
    on<PlanItem[]>("island://plans", (list) => setPlans(list ?? []));
    on<Omit<Toast, "id">>("island://toast", (p) => {
      setToast({ ...p, id: Date.now() + Math.random() });
      if (p.kind !== "timer") cue("notify");
    });
    on<{ cue: Cue }>("soul://sfx", (p) => cue(p.cue));
    on<TextResult>("island://text-result", (r) => {
      setTextResult(r);
      collapse(); // the card is a collapsed-mode stage
      cue("notify");
    });
    on<{ active: boolean; expiresAtMs: number }>("island://context", (c) =>
      setContextUntil(c.active && c.expiresAtMs > Date.now() ? c.expiresAtMs : null),
    );
    on<WakeMicState>("soul://wake", setWake);
    on<{ name: string; label?: string | null; phase: ToolChip["phase"] }>("island://tool", (c) =>
      setTools((prev): ToolChip[] => {
        if (c.phase === "start")
          return [...prev, { id: Date.now() + Math.random(), name: c.name, label: c.label, phase: "start" as const }].slice(-6);
        const idx = [...prev].reverse().findIndex((x) => x.name === c.name && (x.label ?? null) === (c.label ?? null) && x.phase === "start");
        if (idx < 0) return prev;
        const real = prev.length - 1 - idx;
        return prev.map((x, i) => (i === real ? { ...x, phase: c.phase } : x));
      }),
    );
    on<IslandSections>("island://settings", (sec) => {
      setSections(sec);
      void loadSnapshot(false);
    });
    on<boolean>("island://force-collapse", () => {
      collapse();
      const c = collapsedRef.current;
      invoke("island_set_mode", { mode: "collapsed", width: c.w, height: c.h }).catch(() => undefined);
    });

    return () => {
      alive = false;
      window.clearInterval(tick);
      unsubs.forEach((u) => u());
    };
  }, [applyNowPlaying, collapse, cue]);

  // ---------- derived state ----------
  const mediaActive = np.available && Boolean(np.title);
  const active = shown.state !== "idle";
  const kind = shown.kind ?? null;
  const quiet = cfg.noise.quiet;
  const expandAllowed =
    !quiet &&
    (kind === "dictation" ? cfg.noise.expandOnDictation : kind === "command" ? cfg.noise.expandOnCommand : true);
  const showCaption = Boolean(caption) && anim.on("caption") && !quiet;

  const stage: Stage =
    shown.state === "confirm"
      ? "confirm"
      : textResult
        ? "result"
        : toast?.kind === "timer"
        ? "toast"
        : active && !(shown.state === "done" && kind === "dictation" && showCaption)
          ? expandAllowed
            ? "stage"
            : "compact"
          : toast
            ? "toast"
            : updateNotice
              ? "update"
              : showCaption
              ? "caption"
              : mediaActive
                ? "music"
                : "idle";
  const nextTimer = timers[0] ?? null;
  holdCard.current = stage === "result" || (stage === "toast" && Boolean(toast?.action));

  // Announce a new version once per version, only while Soul is quiet: never over a recording,
  // a spoken reply, a confirmation, a toast or a dictation caption (it waits for the next idle).
  const pending = hasPendingUpdate(update);
  const quietNow = !active && !toast && !textResult && !showCaption && mode === "collapsed";
  useEffect(() => {
    if (!pending || !update?.version || announced.current === update.version || !quietNow || quiet) return;
    announced.current = update.version;
    setUpdateNotice(update.version);
    cue("notify");
  }, [pending, update?.version, quietNow, quiet, cue]);
  useEffect(() => {
    if (!updateNotice) return;
    const id = window.setTimeout(() => setUpdateNotice(null), UPDATE_NOTICE_MS);
    return () => window.clearTimeout(id);
  }, [updateNotice]);
  useEffect(() => {
    if (!pending) setUpdateNotice(null);
  }, [pending]);
  const onInstallUpdate = useCallback(() => {
    setUpdateNotice(null);
    installUpdate(update).catch(() => undefined);
  }, [update]);

  const onToastAction = useCallback((t: Toast) => {
    const a = t.action;
    setToast(null);
    if (!a || !TOAST_COMMANDS.has(a.command)) return;
    invoke(a.command, a.args).catch(() => undefined);
  }, []);

  const albumAccent = useAlbumAccent(np.artworkDataUrl, anim.on("albumTint") && mediaActive);
  const baseAccent = albumAccent ?? cfg.accent;
  const accent = active ? accentFor(kind, baseAccent) : baseAccent;
  const spiritColor =
    shown.state === "error" ? tokens.danger : shown.state === "done" ? tokens.success : accent;
  const sState = spiritStateOf(shown, mediaActive && np.playing);

  // Pause decorative loops when collapsed and idle for a while.
  const busy =
    mode !== "collapsed" ||
    active ||
    showCaption ||
    np.playing ||
    timers.length > 0 ||
    Boolean(toast) ||
    Boolean(textResult) ||
    Boolean(updateNotice);
  useEffect(() => {
    if (!cfg.perf.pauseWhenIdle || busy) {
      setPaused(false);
      return;
    }
    const id = window.setTimeout(() => setPaused(true), IDLE_PAUSE_MS);
    return () => window.clearTimeout(id);
  }, [busy, cfg.perf.pauseWhenIdle]);

  const longMessage = (shown.message?.length ?? 0) > 64 && (shown.state === "speaking" || shown.state === "done");
  const collapsedSize =
    stage === "stage" ? (longMessage ? SIZE.stageLong : SIZE.stage) : SIZE[stage];
  const panel =
    mode === "expanded"
      ? SIZE.expanded
      : mode === "peek"
        ? mediaActive
          ? SIZE.peekMusic
          : SIZE.peekIdle
        : collapsedSize;

  const collapsedRef = useRef<{ w: number; h: number }>(collapsedSize);
  collapsedRef.current = collapsedSize;

  // Native window follows: grow now (room for the spring), shrink after it settles.
  const sent = useRef({ w: 0, h: 0 });
  const shrinkDelay = anim.on("morph") && anim.level !== "off" ? Math.round(anim.dur(420) * 1000) : 30;
  useEffect(() => {
    const send = () => {
      sent.current = { w: panel.w, h: panel.h };
      invoke("island_set_mode", { mode, width: panel.w, height: panel.h }).catch(() => undefined);
    };
    if (sent.current.w === 0 || panel.w > sent.current.w || panel.h > sent.current.h) {
      send();
      return;
    }
    const id = window.setTimeout(send, shrinkDelay);
    return () => window.clearTimeout(id);
  }, [mode, panel.w, panel.h, shrinkDelay]);

  // Esc on a pending confirmation = Cancel (safe default).
  useEffect(() => {
    if (stage !== "confirm") return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") invoke("agent_confirm", { accepted: false }).catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stage]);

  const onMedia = useCallback(
    async (action: string) => {
      try {
        await invoke("island_media_action", { action });
        applyNowPlaying(await invoke<NowPlaying>("island_get_now_playing"));
      } catch {
        /* ignore */
      }
    },
    [applyNowPlaying],
  );

  const tabs = useMemo(
    () =>
      (
        [
          { id: "now" as const, label: t("tab.now"), show: sections.nowPlaying },
          { id: "clipboard" as const, label: t("tab.clipboard"), show: sections.clipboard },
          { id: "plans" as const, label: t("tab.plans"), show: sections.plans },
          { id: "apps" as const, label: t("tab.apps"), show: sections.quickApps },
          { id: "settings" as const, label: t("tab.settings"), show: sections.quickSettings },
        ] satisfies { id: Tab; label: string; show: boolean }[]
      ).filter((x) => x.show),
    [sections, t],
  );

  const morph = anim.on("morph") && anim.level !== "off";
  const fade = {
    initial: { opacity: 0, scale: morph ? 0.96 : 1, filter: morph ? "blur(4px)" : "blur(0px)" },
    animate: { opacity: 1, scale: 1, filter: "blur(0px)" },
    exit: { opacity: 0, scale: morph ? 0.98 : 1, filter: morph ? "blur(3px)" : "blur(0px)" },
    transition: { duration: anim.dur(morph ? 200 : 90) },
  };
  const contentKey = mode === "collapsed" ? `c-${stage}` : mode;
  const spiritLayout = morph ? "soul-spirit" : undefined;

  const statusLabel = labelFor(shown, t);

  return (
    <div
      className="flex h-full w-full items-start bg-transparent"
      style={{
        justifyContent: align === "center" ? "center" : align === "start" ? "flex-start" : "flex-end",
        padding: `0 ${PAD_X}px ${PAD_B}px`,
      }}
    >
      <motion.div
        {...handlers}
        initial={false}
        animate={{ width: panel.w, height: panel.h, borderRadius: mode === "collapsed" ? collapsedSize.r : panel.r }}
        transition={anim.spring("morph")}
        className="island-glass relative shrink-0 text-white"
        style={{ ["--accent" as string]: accent }}
        onClick={() => {
          if (mode === "collapsed" && stage !== "confirm" && stage !== "result") expand();
        }}
      >
        <LayoutGroup>
          <AnimatePresence initial={false} mode="popLayout">
            <motion.div
              key={contentKey}
              {...fade}
              className="absolute left-1/2 top-0"
              style={{ width: panel.w, height: panel.h, x: "-50%" }}
            >
              {mode === "expanded" ? (
                <Expanded
                  spirit={<SpiritOrDot state={sState} color={spiritColor} size={30} paused={paused} />}
                  spiritLayout={spiritLayout}
                  title={
                    active
                      ? statusLabel
                      : mediaActive
                        ? np.title
                        : "Soul"
                  }
                  subtitle={
                    active
                      ? (shown.message ?? kindLabel(kind, t))
                      : mediaActive
                        ? np.artist
                        : clock
                  }
                  chip={active ? kindLabel(kind, t) || statusLabel : mediaActive ? t("status.music") : t("status.ready")}
                  wave={shown.state === "listening" ? "mic" : shown.state === "speaking" ? "voice" : null}
                  accent={accent}
                  onHeader={collapse}
                  tabs={tabs}
                  tab={tab}
                  setTab={setTab}
                >
                  {tab === "now" && <NowPlayingPanel np={np} anchor={anchor} onMedia={onMedia} accent={accent} />}
                  {tab === "clipboard" && (
                    <ClipboardPanel
                      items={clipboard}
                      onClear={() => {
                        invoke("island_clipboard_clear").catch(() => undefined);
                        setClipboard([]);
                      }}
                    />
                  )}
                  {tab === "plans" && (
                    <PlansPanel
                      plans={plans}
                      timers={timers}
                      nowMs={nowMs}
                      onCancelTimer={(id) => invoke("timer_cancel", { id }).catch(() => undefined)}
                    />
                  )}
                  {tab === "apps" && <AppsPanel pinned={pinned} setPinned={setPinned} />}
                  {tab === "settings" && <QuickSettingsPanel />}
                </Expanded>
              ) : mode === "peek" ? (
                <Peek
                  spirit={<SpiritOrDot state={sState} color={spiritColor} size={22} paused={paused} />}
                  spiritLayout={spiritLayout}
                  np={np}
                  media={mediaActive}
                  onMedia={onMedia}
                  status={active ? `${statusLabel}${shown.message ? ` — ${shown.message}` : ""}` : null}
                  clock={clock}
                  onOpen={toggle}
                  update={pending ? update : null}
                  onInstallUpdate={onInstallUpdate}
                />
              ) : (
                <Collapsed
                  stage={stage}
                  spirit={(size: number) => (
                    <SpiritOrDot state={sState} color={spiritColor} size={size} paused={paused} />
                  )}
                  spiritLayout={spiritLayout}
                  banner={shown}
                  label={statusLabel}
                  kindText={kindLabel(kind, t)}
                  accent={accent}
                  np={np}
                  clock={clock}
                  caption={caption}
                  tools={tools}
                  toast={toast}
                  onToastAction={onToastAction}
                  onToastClose={() => setToast(null)}
                  textResult={textResult}
                  onResultClose={() => setTextResult(null)}
                  contextActive={contextUntil !== null}
                  timer={nextTimer}
                  nowMs={nowMs}
                  wake={wake}
                  update={update}
                  onInstallUpdate={onInstallUpdate}
                  onUpdateClose={() => setUpdateNotice(null)}
                />
              )}
            </motion.div>
          </AnimatePresence>
        </LayoutGroup>
      </motion.div>
    </div>
  );
}

function labelFor(b: Banner, t: ReturnType<typeof useT>): string {
  switch (b.state) {
    case "listening":
      return b.kind === "dictation" ? t("island.dictation") : b.kind === "command" ? t("island.command") : t("island.listening");
    case "processing":
      return t("island.processing");
    case "thinking":
      return t("island.thinking");
    case "retrying":
      return t("island.retrying");
    case "speaking":
      return t("island.speaking");
    case "done":
      return t("island.done");
    case "error":
      return t("island.error");
    case "confirm":
      return t("island.confirmTitle");
    default:
      return "";
  }
}

function kindLabel(kind: ModeKind | null, t: ReturnType<typeof useT>): string {
  if (kind === "dictation") return t("island.dictation");
  if (kind === "command") return t("island.command");
  if (kind === "ask") return t("island.ask");
  return "";
}

/** Spirit wrapper that glides between stages via a shared layoutId. */
function SpiritSlot({ layoutId, children }: { layoutId?: string; children: React.ReactNode }) {
  const anim = useAnim();
  return (
    <motion.div layoutId={layoutId} transition={anim.spring("morph")} className="flex shrink-0 items-center justify-center">
      {children}
    </motion.div>
  );
}

const TOOL_LABELS: Record<string, [string, string]> = {
  open_app: ["Otwieram", "Opening"],
  close_app: ["Zamykam", "Closing"],
  focus_app: ["Przełączam", "Focusing"],
  open_url: ["Link", "Link"],
  web_search: ["Szukam", "Searching"],
  youtube_search: ["YouTube", "YouTube"],
  youtube_play: ["YouTube ▶", "YouTube ▶"],
  set_volume: ["Głośność", "Volume"],
  media_control: ["Media", "Media"],
  type_text: ["Piszę", "Typing"],
  set_voice_volume: ["Głos", "Voice"],
  remember: ["Pamięć", "Memory"],
  forget: ["Zapominam", "Forgetting"],
  recall: ["Pamięć", "Memory"],
  add_note: ["Notatka", "Note"],
  set_timer: ["Timer", "Timer"],
  cancel_timer: ["Timer ✕", "Timer ✕"],
  run_scene: ["Scena", "Scene"],
  text_action: ["Tekst", "Text"],
  create_scene: ["Nowa scena", "New scene"],
  now_playing: ["Co gra", "Now playing"],
  morning_briefing: ["Briefing", "Briefing"],
  media: ["Muzyka", "Media"],
  delay: ["Czekam", "Waiting"],
  dnd: ["Nie przeszkadzać", "Do not disturb"],
};

function ToolChips({ tools }: { tools: ToolChip[] }) {
  const anim = useAnim();
  const lang = anim.cfg.uiLang;
  return (
    <div className="flex flex-wrap gap-1">
      <AnimatePresence initial={false}>
        {tools.map((c) => (
          <motion.span
            key={c.id}
            layout={anim.on("micro")}
            initial={{ opacity: 0, scale: 0.6, y: 4 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.8 }}
            transition={anim.spring("bouncy")}
            title={c.name}
            className="inline-flex items-center gap-1 rounded-full border border-white/10 bg-white/8 px-2 py-0.5 text-[10.5px] text-white/85"
          >
            {c.phase === "start" ? (
              <span className="soul-spin h-2 w-2 rounded-full border border-[var(--accent)] border-t-transparent" />
            ) : c.phase === "ok" ? (
              <span className="text-[var(--soul-success)]">✓</span>
            ) : (
              <span className="text-[var(--soul-danger)]">✕</span>
            )}
            {c.label || (TOOL_LABELS[c.name] ?? [c.name, c.name])[lang === "en" ? 1 : 0]}
          </motion.span>
        ))}
      </AnimatePresence>
    </div>
  );
}

function Collapsed({
  stage,
  spirit,
  spiritLayout,
  banner,
  label,
  kindText,
  accent,
  np,
  clock,
  caption,
  tools,
  toast,
  onToastAction,
  onToastClose,
  textResult,
  onResultClose,
  contextActive,
  timer,
  nowMs,
  wake,
  update,
  onInstallUpdate,
  onUpdateClose,
}: {
  textResult: TextResult | null;
  onResultClose: () => void;
  contextActive: boolean;
  wake: WakeMicState | null;
  update: UpdateStatus | null;
  onInstallUpdate: () => void;
  onUpdateClose: () => void;
  toast: Toast | null;
  onToastAction: (t: Toast) => void;
  onToastClose: () => void;
  timer: TimerItem | null;
  nowMs: number;
  stage: Stage;
  spirit: (size: number) => React.ReactNode;
  spiritLayout?: string;
  banner: Banner;
  label: string;
  kindText: string;
  accent: string;
  np: NowPlaying;
  clock: string;
  caption: string | null;
  tools: ToolChip[];
}) {
  const t = useT();
  const wave = banner.state === "listening" ? "mic" : banner.state === "speaking" ? "voice" : null;

  if (stage === "confirm") {
    return (
      <div className="flex h-full flex-col justify-between px-4 pb-3.5 pt-3" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center gap-3">
          <SpiritSlot layoutId={spiritLayout}>{spirit(28)}</SpiritSlot>
          <div className="min-w-0">
            <div className="text-[13px] font-semibold">{t("island.confirmTitle")}</div>
            <div className="truncate text-[12px] text-[var(--soul-muted)]">{banner.message}</div>
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Press
            onClick={() => invoke("agent_confirm", { accepted: false }).catch(() => undefined)}
            className="soul-btn-ghost rounded-full px-4 py-1.5 text-[12px]"
          >
            {t("island.cancel")}
          </Press>
          <Press
            onClick={() => invoke("agent_confirm", { accepted: true }).catch(() => undefined)}
            className="rounded-full px-4 py-1.5 text-[12px] font-semibold text-[#1a0b0d]"
            style={{ background: tokens.danger }}
          >
            {t("island.confirm")}
          </Press>
        </div>
      </div>
    );
  }

  if (stage === "result" && textResult) {
    return <TextResultCard result={textResult} spirit={spirit(26)} spiritLayout={spiritLayout} accent={accent} onClose={onResultClose} />;
  }

  if (stage === "toast" && toast) {
    const color =
      toast.kind === "timer" ? tokens.accentDictation : toast.kind === "scene" ? tokens.accentAsk : toast.kind === "memory" ? tokens.success : accent;
    return (
      <div className="flex h-full items-center gap-3 px-4" onClick={(e) => e.stopPropagation()}>
        <SpiritSlot layoutId={spiritLayout}>{spirit(28)}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="text-[10.5px] font-medium uppercase tracking-wider" style={{ color }}>
            {toast.kind === "timer"
              ? "⏰ Timer"
              : toast.kind === "scene"
                ? t("island.command")
                : toast.kind === "note"
                  ? "✎"
                  : toast.kind === "wake"
                    ? "Soul"
                    : toast.kind === "text"
                      ? `✓ ${t("island.textResult")}`
                      : "✓"}
          </div>
          <div className="line-clamp-2 text-[12.5px] leading-snug text-white/90">{toast.text}</div>
        </div>
        {toast.action && (
          <Press
            onClick={() => onToastAction(toast)}
            className="shrink-0 rounded-full px-3 py-1.5 text-[11.5px] font-semibold text-[#0b0d0c]"
            style={{ background: color }}
          >
            {toast.action.label}
          </Press>
        )}
        <Press onClick={onToastClose} className="soul-btn-ghost shrink-0 rounded-full px-2 py-1 text-[11px]" aria-label="Zamknij">
          ✕
        </Press>
      </div>
    );
  }

  if (stage === "update" && update?.version) {
    return (
      <div className="flex h-full items-center gap-3 px-4" onClick={(e) => e.stopPropagation()}>
        <SpiritSlot layoutId={spiritLayout}>{spirit(28)}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="text-[13px] font-semibold tracking-tight">{t("update.newVersion", { v: update.version })}</div>
          <div className="truncate text-[11.5px] text-white/60">
            {update.install === "manual" ? t("update.hintManual") : t("update.hint")}
          </div>
        </div>
        <UpdateButton update={update} onClick={onInstallUpdate} accent={accent} />
        <Press onClick={onUpdateClose} className="soul-btn-ghost shrink-0 rounded-full px-2 py-1 text-[11px]" aria-label="Zamknij">
          ✕
        </Press>
      </div>
    );
  }

  if (stage === "stage") {
    const long = (banner.message?.length ?? 0) > 64 && (banner.state === "speaking" || banner.state === "done");
    return (
      <div className="flex h-full items-center gap-3 px-4">
        <SpiritSlot layoutId={spiritLayout}>{spirit(30)}</SpiritSlot>
        <div className="flex min-w-0 flex-1 flex-col justify-center gap-0.5">
          <div className="flex items-center gap-2">
            <span className="text-[13px] font-semibold tracking-tight" style={{ color: banner.state === "error" ? tokens.danger : undefined }}>
              {label}
            </span>
            {kindText && banner.state !== "listening" && (
              <span className="rounded-full px-1.5 py-px text-[9.5px] font-medium uppercase tracking-wider" style={{ color: accent, background: "rgb(255 255 255 / 0.06)" }}>
                {kindText}
              </span>
            )}
          </div>
          {banner.message && (
            <div className={`text-[12px] leading-snug text-white/70 ${long ? "line-clamp-3" : "truncate"}`}>{banner.message}</div>
          )}
          {tools.length > 0 && <ToolChips tools={tools} />}
        </div>
        {wave && <Waveform source={wave} color={accent} bars={14} height={26} />}
      </div>
    );
  }

  if (stage === "compact") {
    return (
      <div className="flex h-full items-center gap-2 px-2.5">
        <SpiritSlot layoutId={spiritLayout}>{spirit(18)}</SpiritSlot>
        <span className="min-w-0 flex-1 truncate text-[12px] font-medium text-white/90">{label}</span>
        {wave && <Waveform source={wave} color={accent} bars={8} height={14} />}
      </div>
    );
  }

  if (stage === "caption") {
    return (
      <div className="flex h-full items-center gap-3 px-4">
        <SpiritSlot layoutId={spiritLayout}>{spirit(30)}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="text-[10.5px] font-medium uppercase tracking-wider" style={{ color: tokens.success }}>
            ✓ {t("island.pasted")}
          </div>
          <div className="line-clamp-2 text-[12.5px] leading-snug text-white/85">{caption}</div>
        </div>
      </div>
    );
  }

  if (stage === "music") {
    return (
      <div className="flex h-full items-center gap-2 px-2.5">
        <SpiritSlot layoutId={spiritLayout}>{spirit(18)}</SpiritSlot>
        <span className="shrink-0 text-[11px] font-medium tabular-nums text-white/60">
          {timer ? `⏱ ${countdown(timer.endsAtMs - nowMs)}` : clock}
        </span>
        <Marquee text={np.title} className="flex-1 text-[12px] font-medium text-white/90" />
        <ContextDot active={contextActive} />
        <WakeMic wake={wake} />
        <EqBars playing={np.playing} color={accent} />
        <Artwork src={np.artworkDataUrl} size={22} radius={6} />
      </div>
    );
  }

  return (
    <div className="flex h-full items-center gap-2 px-2.5">
      <SpiritSlot layoutId={spiritLayout}>{spirit(18)}</SpiritSlot>
      {timer ? (
        <span className="flex min-w-0 flex-1 items-center justify-center gap-1.5 text-[12px] font-medium tabular-nums text-white/85">
          <span style={{ color: tokens.accentDictation }}>⏱ {countdown(timer.endsAtMs - nowMs)}</span>
          {timer.label && <span className="truncate text-white/55">{timer.label}</span>}
        </span>
      ) : (
        <span className="flex-1 text-center text-[12px] font-medium tabular-nums tracking-wide text-white/80">{clock}</span>
      )}
      <span className="flex w-8 items-center justify-end gap-1.5 pr-0.5">
        <ContextDot active={contextActive} />
        <WakeMic wake={wake} />
      </span>
    </div>
  );
}

/** Subtle dot while the conversation context is alive (follow-ups route to commands). */
function ContextDot({ active }: { active: boolean }) {
  const t = useT();
  if (!active) return null;
  const title = t("island.contextActive");
  return (
    <span
      title={title}
      aria-label={title}
      className="inline-block h-1.5 w-1.5 shrink-0 rounded-full"
      style={{ background: tokens.accentCommand, opacity: 0.75, boxShadow: `0 0 6px ${tokens.accentCommand}` }}
    />
  );
}

/** Summary / explanation of the selected text with „Kopiuj” / „Wklej”. */
function TextResultCard({
  result,
  spirit,
  spiritLayout,
  accent,
  onClose,
}: {
  result: TextResult;
  spirit: React.ReactNode;
  spiritLayout?: string;
  accent: string;
  onClose: () => void;
}) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  useEffect(() => setCopied(false), [result.id]);
  return (
    <div className="flex h-full flex-col px-4 pb-3 pt-3" onClick={(e) => e.stopPropagation()}>
      <div className="flex items-center gap-3">
        <SpiritSlot layoutId={spiritLayout}>{spirit}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="text-[10.5px] font-medium uppercase tracking-wider" style={{ color: accent }}>
            {t("island.textResult")}
          </div>
          <div className="truncate text-[13px] font-semibold tracking-tight">{result.title}</div>
        </div>
        <Press onClick={onClose} className="soul-btn-ghost shrink-0 rounded-full px-2 py-1 text-[11px]" aria-label="Zamknij">
          ✕
        </Press>
      </div>
      <div className="soul-scroll mt-2 min-h-0 flex-1 select-text overflow-auto whitespace-pre-wrap pr-1 text-[12.5px] leading-snug text-white/85">
        {result.text}
      </div>
      <div className="mt-2 flex justify-end gap-2">
        <Press
          onClick={() =>
            invoke("text_result_copy", { id: result.id })
              .then(() => setCopied(true))
              .catch(() => undefined)
          }
          className="soul-btn-ghost rounded-full px-4 py-1.5 text-[12px]"
        >
          {copied ? `✓ ${t("island.copied")}` : t("island.copy")}
        </Press>
        <Press
          onClick={() => {
            invoke("text_result_paste", { id: result.id }).catch(() => undefined);
            onClose();
          }}
          className="rounded-full px-4 py-1.5 text-[12px] font-semibold text-[#0b0d0c]"
          style={{ background: accent }}
        >
          {t("island.paste")}
        </Press>
      </div>
    </div>
  );
}

/** `wake::WakeStatus` (only the fields the Island uses). */
type WakeMicState = { enabled: boolean; trained: boolean; listening: boolean; paused: string | null };

/** Tiny mic glyph while the offline wake word is on: bright = listening, dim = paused. */
function WakeMic({ wake }: { wake: WakeMicState | null }) {
  const lang = useAnim().cfg.uiLang;
  if (!wake?.enabled || !wake.trained) return null;
  const title = wake.listening
    ? tr(lang, "Nasłuchuję słowa „Soul” (lokalnie)", "Listening for “Soul” (on-device)")
    : tr(lang, `Słowo „Soul” wstrzymane (${wake.paused ?? ""})`, `“Soul” paused (${wake.paused ?? ""})`);
  return (
    <span title={title} aria-label={title} className="inline-flex shrink-0 items-center" style={{ opacity: wake.listening ? 0.85 : 0.3 }}>
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
      </svg>
    </span>
  );
}

function Peek({
  spirit,
  spiritLayout,
  np,
  media,
  onMedia,
  status,
  clock,
  onOpen,
  update,
  onInstallUpdate,
}: {
  spirit: React.ReactNode;
  spiritLayout?: string;
  np: NowPlaying;
  media: boolean;
  onMedia: (a: string) => void;
  status: string | null;
  clock: string;
  onOpen: () => void;
  update: UpdateStatus | null;
  onInstallUpdate: () => void;
}) {
  const t = useT();
  const anim = useAnim();
  const updateLine = update?.version ? t("update.newVersion", { v: update.version }) : null;
  if (!media) {
    return (
      <div className="flex h-full cursor-default items-center gap-3 px-3.5" onClick={onOpen}>
        <SpiritSlot layoutId={spiritLayout}>{spirit}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="text-[13px] font-semibold tabular-nums">{clock}</div>
          <div className="truncate text-[11px] text-[var(--soul-muted)]">{status ?? updateLine ?? t("island.idleHint")}</div>
        </div>
        {update && !status && <UpdateButton update={update} onClick={onInstallUpdate} accent={anim.cfg.accent} />}
      </div>
    );
  }
  return (
    <div className="flex h-full items-center gap-3 px-3.5" onClick={onOpen}>
      <SpiritSlot layoutId={spiritLayout}>{spirit}</SpiritSlot>
      <Artwork src={np.artworkDataUrl} size={46} radius={11} />
      <div className="min-w-0 flex-1">
        <Marquee text={np.title} className="text-[13px] font-semibold" />
        <div className="truncate text-[11px] text-[var(--soul-muted)]">{status ?? updateLine ?? np.artist}</div>
      </div>
      {update && !status && <UpdateButton update={update} onClick={onInstallUpdate} accent={anim.cfg.accent} />}
      <Transport playing={np.playing} onMedia={onMedia} size={30} />
    </div>
  );
}

/** "Zainstaluj" / "Pobierz" (package-manager install) / "Instaluję…" pill. */
function UpdateButton({ update, onClick, accent }: { update: UpdateStatus; onClick: () => void; accent: string }) {
  const t = useT();
  const busy = update.phase === "waiting" || update.phase === "installing" || update.phase === "downloading";
  const label = busy
    ? t("update.installing")
    : update.install === "manual"
      ? t("update.download")
      : update.phase === "installed"
        ? t("update.restart")
        : t("update.install");
  return (
    <Press
      onClick={onClick}
      disabled={busy}
      className="shrink-0 rounded-full px-3 py-1.5 text-[11.5px] font-semibold text-[#0b0d0c] disabled:opacity-60"
      style={{ background: accent }}
    >
      {label}
    </Press>
  );
}

function Expanded({
  spirit,
  spiritLayout,
  title,
  subtitle,
  chip,
  wave,
  accent,
  onHeader,
  tabs,
  tab,
  setTab,
  children,
}: {
  spirit: React.ReactNode;
  spiritLayout?: string;
  title: string;
  subtitle: string;
  chip: string;
  wave: "mic" | "voice" | null;
  accent: string;
  onHeader: () => void;
  tabs: { id: Tab; label: string }[];
  tab: Tab;
  setTab: (t: Tab) => void;
  children: React.ReactNode;
}) {
  const anim = useAnim();
  const tabAnim = anim.on("tabs");
  return (
    <div className="flex h-full flex-col px-4 pb-3.5 pt-3">
      <div className="flex cursor-default items-center gap-3" onClick={onHeader}>
        <SpiritSlot layoutId={spiritLayout}>{spirit}</SpiritSlot>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[16px] font-semibold tracking-tight">{title}</div>
          {wave ? (
            <Waveform source={wave} color={accent} bars={22} height={16} className="mt-1" />
          ) : (
            <div className="truncate text-[12px] text-[var(--soul-muted)]">{subtitle}</div>
          )}
        </div>
        <span
          className="shrink-0 rounded-full border border-white/10 px-2.5 py-1 text-[10.5px] font-medium"
          style={{ color: accent, background: "rgb(255 255 255 / 0.05)" }}
        >
          {chip}
        </span>
      </div>

      <div className="relative mt-3 flex gap-0.5 rounded-full bg-white/5 p-0.5" role="tablist">
        {tabs.map((x) => (
          <button
            key={x.id}
            type="button"
            role="tab"
            aria-selected={tab === x.id}
            onClick={() => setTab(x.id)}
            className={`relative flex-1 rounded-full px-2 py-1 text-[11px] transition-colors ${
              tab === x.id ? "text-white" : "text-white/45 hover:text-white/80"
            }`}
          >
            {tab === x.id && (
              <motion.span
                layoutId={tabAnim ? "island-tab" : undefined}
                transition={anim.spring("ui")}
                className="absolute inset-0 rounded-full bg-white/12"
              />
            )}
            <span className="relative">{x.label}</span>
          </button>
        ))}
      </div>

      <div className="relative mt-3 min-h-0 flex-1">
        <AnimatePresence initial={false} mode="wait">
          <motion.div
            key={tab}
            initial={tabAnim ? { opacity: 0, y: 6 } : false}
            animate={{ opacity: 1, y: 0 }}
            exit={tabAnim ? { opacity: 0, y: -4 } : { opacity: 0 }}
            transition={{ duration: anim.dur(tabAnim ? 160 : 0) }}
            className="soul-scroll absolute inset-0 overflow-auto pr-1"
          >
            {children}
          </motion.div>
        </AnimatePresence>
      </div>
    </div>
  );
}

export type { IslandMode };
