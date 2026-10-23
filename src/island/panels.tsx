import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AnimatePresence, motion } from "framer-motion";
import { useAnim, useAppearance, useT } from "../anim/AnimationProvider";
import { NextIcon, PauseIcon, PlayIcon, PrevIcon, Press, ProgressBar } from "./bits";
import type { ProgressAnchor } from "./progress";

export interface NowPlaying {
  title: string;
  artist: string;
  album: string;
  artworkDataUrl: string;
  playing: boolean;
  available: boolean;
  positionMs: number;
  durationMs: number;
  positionUpdatedAtMs?: number;
  playbackRate?: number;
}

export interface ClipboardItem {
  id: string;
  text: string;
  preview: string;
  ts: number;
}

export interface PlanItem {
  id: string;
  title: string;
  time: string;
  done: boolean;
}

interface AppSuggestion {
  name: string;
  iconDataUrl: string;
}

export function formatTime(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "0:00";
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

function letterIcon(name: string): string {
  const letter = (name.match(/[\p{L}0-9]/u)?.[0] ?? "?").toUpperCase();
  let hue = 0;
  for (let i = 0; i < name.length; i++) hue = (hue * 31 + name.charCodeAt(i)) >>> 0;
  hue %= 360;
  const svg = `<svg xmlns='http://www.w3.org/2000/svg' width='64' height='64' viewBox='0 0 64 64'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='hsl(${hue} 55% 52%)'/><stop offset='1' stop-color='hsl(${(hue + 40) % 360} 50% 34%)'/></linearGradient></defs><rect width='64' height='64' rx='16' fill='url(#g)'/><text x='32' y='42' text-anchor='middle' font-family='Segoe UI,system-ui,sans-serif' font-size='28' font-weight='600' fill='white'>${letter}</text></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

export function Artwork({ src, size, radius = 10 }: { src: string; size: number; radius?: number }) {
  return (
    <div
      className="shrink-0 overflow-hidden bg-white/8"
      style={{ width: size, height: size, borderRadius: radius }}
    >
      {src ? (
        <img src={src} alt="" className="h-full w-full object-cover" draggable={false} />
      ) : (
        <div className="flex h-full items-center justify-center text-white/30">♪</div>
      )}
    </div>
  );
}

export function Transport({
  playing,
  onMedia,
  size = 32,
}: {
  playing: boolean;
  onMedia: (a: string) => void;
  size?: number;
}) {
  const t = useT();
  const btn = "inline-flex items-center justify-center rounded-full";
  return (
    <div className="flex items-center gap-1.5">
      <Press label={t("media.prev")} onClick={() => onMedia("prev")} className={`${btn} soul-btn-ghost`} style={{ width: size, height: size }}>
        <PrevIcon />
      </Press>
      <Press
        label={playing ? t("media.pause") : t("media.play")}
        onClick={() => onMedia(playing ? "pause" : "play")}
        className={`${btn} soul-btn-accent`}
        style={{ width: size + 4, height: size + 4 }}
      >
        {playing ? <PauseIcon /> : <PlayIcon />}
      </Press>
      <Press label={t("media.next")} onClick={() => onMedia("next")} className={`${btn} soul-btn-ghost`} style={{ width: size, height: size }}>
        <NextIcon />
      </Press>
    </div>
  );
}

export function NowPlayingPanel({
  np,
  anchor,
  onMedia,
  accent,
}: {
  np: NowPlaying;
  anchor: ProgressAnchor | null;
  onMedia: (a: string) => void;
  accent: string;
}) {
  const t = useT();
  const [elapsed, setElapsed] = useState(np.positionMs || 0);
  if (!np.available) {
    return <p className="pt-10 text-center text-xs text-[var(--soul-muted)]">{t("island.nothingPlaying")}</p>;
  }
  return (
    <div className="flex gap-4 pt-1">
      <div
        className="shrink-0 rounded-2xl"
        style={{ boxShadow: `0 8px 22px -10px ${accent}`, transition: "box-shadow 0.6s" }}
      >
        <Artwork src={np.artworkDataUrl} size={112} radius={16} />
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="truncate text-[15px] font-semibold tracking-tight">{np.title || "—"}</div>
        <div className="mt-0.5 truncate text-[12px] text-[var(--soul-muted)]">
          {np.artist || t("island.unknownArtist")}
        </div>
        {np.album && <div className="mt-0.5 truncate text-[11px] text-white/35">{np.album}</div>}
        <div className="mt-auto">
          <ProgressBar
            anchor={anchor}
            durationMs={np.durationMs}
            fallbackMs={np.positionMs || 0}
            playing={np.playing}
            color={accent}
            onTime={setElapsed}
          />
          <div className="mt-1 flex justify-between text-[10px] tabular-nums text-white/40">
            <span>{formatTime(elapsed)}</span>
            <span>{np.durationMs > 0 ? formatTime(np.durationMs) : "--:--"}</span>
          </div>
          <div className="mt-1.5 flex justify-center">
            <Transport playing={np.playing} onMedia={onMedia} />
          </div>
        </div>
      </div>
    </div>
  );
}

export function ClipboardPanel({ items, onClear }: { items: ClipboardItem[]; onClear: () => void }) {
  const t = useT();
  const anim = useAnim();
  const [copied, setCopied] = useState<string | null>(null);
  const copy = (id: string) => {
    invoke("island_clipboard_copy", { id }).catch(() => undefined);
    setCopied(id);
    window.setTimeout(() => setCopied((c) => (c === id ? null : c)), 1100);
  };
  return (
    <div>
      <div className="mb-2 flex items-center justify-between">
        <span className="text-[11px] text-[var(--soul-muted)]">{t("clip.last", { n: items.length })}</span>
        <Press onClick={onClear} className="rounded-md px-2 py-0.5 text-[11px] text-white/45 hover:bg-white/6 hover:text-white">
          {t("clip.clear")}
        </Press>
      </div>
      {items.length === 0 ? (
        <p className="pt-6 text-center text-xs text-[var(--soul-muted)]">{t("clip.empty")}</p>
      ) : (
        <ul className="space-y-1">
          <AnimatePresence initial={false}>
            {items.map((item) => (
              <motion.li
                key={item.id}
                layout={anim.on("micro")}
                initial={{ opacity: 0, y: -4 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                transition={anim.spring("ui")}
              >
                <Press
                  onClick={() => copy(item.id)}
                  className="soul-row w-full rounded-xl px-3 py-2 text-left text-[12px] text-white/85"
                  label={item.text.slice(0, 200)}
                >
                  <span className="flex items-start gap-2">
                    <span className="line-clamp-2 min-w-0 flex-1 whitespace-pre-wrap break-words">{item.preview}</span>
                    <span
                      className="shrink-0 text-[10px] text-[var(--accent)] transition-opacity"
                      style={{ opacity: copied === item.id ? 1 : 0 }}
                    >
                      ✓
                    </span>
                  </span>
                </Press>
              </motion.li>
            ))}
          </AnimatePresence>
        </ul>
      )}
    </div>
  );
}

export function PlansPanel({
  plans,
  timers = [],
  nowMs = Date.now(),
  onCancelTimer,
}: {
  plans: PlanItem[];
  timers?: { id: number; label: string; endsAtMs: number; totalMs: number }[];
  nowMs?: number;
  onCancelTimer?: (id: number) => void;
}) {
  const t = useT();
  if (plans.length === 0 && timers.length === 0) {
    return <p className="px-6 pt-10 text-center text-xs text-[var(--soul-muted)]">{t("plans.empty")}</p>;
  }
  const fmt = (ms: number) => {
    const s = Math.max(0, Math.ceil(ms / 1000));
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = String(s % 60).padStart(2, "0");
    return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
  };
  return (
    <div className="space-y-3">
      {timers.length > 0 && (
        <ul className="space-y-1.5">
          <li className="px-1 text-[10px] font-medium uppercase tracking-wider text-[var(--soul-muted)]">{t("plans.timers")}</li>
          {timers.map((tm) => {
            const left = tm.endsAtMs - nowMs;
            const pct = tm.totalMs > 0 ? Math.min(100, Math.max(0, (1 - left / tm.totalMs) * 100)) : 0;
            return (
              <li key={tm.id} className="soul-row relative flex items-center gap-2.5 overflow-hidden rounded-xl px-3 py-2 text-[12px] text-white/85">
                <span className="absolute inset-y-0 left-0 bg-[var(--accent)]/15" style={{ width: `${pct}%` }} />
                <span className="relative tabular-nums text-[12px] font-semibold text-[var(--accent)]">⏱ {fmt(left)}</span>
                <span className="relative min-w-0 flex-1 truncate">{tm.label || "Timer"}</span>
                {onCancelTimer && (
                  <button type="button" onClick={() => onCancelTimer(tm.id)} className="relative text-[11px] text-white/45 hover:text-white" aria-label="Anuluj">
                    ✕
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {plans.length > 0 && (
    <ul className="space-y-1.5">
      <li className="px-1 text-[10px] font-medium uppercase tracking-wider text-[var(--soul-muted)]">{t("plans.notes")}</li>
      {plans.map((p) => (
        <li
          key={p.id}
          className={`flex items-center gap-2.5 rounded-xl px-3 py-2 text-[12px] ${
            p.done ? "bg-white/4 text-white/35 line-through" : "soul-row text-white/85"
          }`}
        >
          <span className="tabular-nums text-[11px] text-[var(--accent)]">{p.time}</span>
          <span className="truncate">{p.title}</span>
        </li>
      ))}
    </ul>
      )}
    </div>
  );
}

export function AppsPanel({ pinned, setPinned }: { pinned: string[]; setPinned: (p: string[]) => void }) {
  const t = useT();
  const [input, setInput] = useState("");
  const [suggestions, setSuggestions] = useState<AppSuggestion[]>([]);
  const [open, setOpen] = useState(false);
  const debounce = useRef<number | null>(null);

  useEffect(() => {
    if (debounce.current) window.clearTimeout(debounce.current);
    const q = input.trim();
    if (!q) {
      setSuggestions([]);
      return;
    }
    debounce.current = window.setTimeout(() => {
      invoke<AppSuggestion[]>("island_search_apps", { query: q, limit: 8 })
        .then((list) => {
          setSuggestions(list);
          setOpen(true);
        })
        .catch(() => setSuggestions([]));
    }, 160);
    return () => {
      if (debounce.current) window.clearTimeout(debounce.current);
    };
  }, [input]);

  const save = (next: string[]) => {
    setPinned(next);
    invoke("island_update_pins", { pinned: next }).catch(() => undefined);
  };
  const add = (nameOverride?: string) => {
    const name = (nameOverride ?? input).trim();
    if (!name) return;
    save([...pinned.filter((p) => p.toLowerCase() !== name.toLowerCase()), name]);
    setInput("");
    setOpen(false);
  };

  return (
    <div>
      <div className="relative mb-3">
        <div className="flex gap-1.5">
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onFocus={() => {
              // The Island window is non-activating; typing needs it to take the keyboard.
              invoke("island_focus_window", { focused: true }).catch(() => undefined);
              if (suggestions.length > 0) setOpen(true);
            }}
            onBlur={() => {
              invoke("island_focus_window", { focused: false }).catch(() => undefined);
              window.setTimeout(() => setOpen(false), 120);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                add(suggestions[0]?.name);
              }
              if (e.key === "Escape") {
                e.stopPropagation();
                setOpen(false);
              }
            }}
            placeholder={t("apps.placeholder")}
            className="soul-input min-w-0 flex-1 px-3 py-1.5 text-[12px]"
          />
          <Press onClick={() => add()} className="soul-btn-ghost rounded-lg px-3 text-[11px]">
            {t("apps.add")}
          </Press>
        </div>
        {open && suggestions.length > 0 && (
          <ul className="absolute left-0 right-14 top-full z-10 mt-1 max-h-36 overflow-auto rounded-xl border border-white/10 bg-[rgb(18_18_21/0.98)] py-1 shadow-xl">
            {suggestions.map((s) => (
              <li key={s.name}>
                <button
                  type="button"
                  className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-[12px] hover:bg-white/8"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => add(s.name)}
                >
                  <img src={s.iconDataUrl || letterIcon(s.name)} alt="" className="h-5 w-5 rounded-md object-cover" />
                  <span className="truncate">{s.name}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      {pinned.length === 0 ? (
        <p className="pt-4 text-center text-xs text-[var(--soul-muted)]">{t("apps.empty")}</p>
      ) : (
        <div className="grid grid-cols-5 gap-2">
          {pinned.map((name) => (
            <div key={name} className="group relative">
              <Press
                onClick={() => invoke("island_launch_pinned", { name }).catch(() => undefined)}
                className="soul-row flex w-full flex-col items-center gap-1.5 rounded-2xl px-1 py-2.5"
                label={name}
              >
                <img src={letterIcon(name)} alt="" className="h-9 w-9 rounded-[10px]" draggable={false} />
                <span className="w-full truncate text-center text-[10.5px] text-white/80">{name}</span>
              </Press>
              <button
                type="button"
                onClick={() => save(pinned.filter((p) => p !== name))}
                className="absolute right-1 top-1 hidden h-4 w-4 items-center justify-center rounded-full bg-black/60 text-[10px] text-white/70 hover:text-white group-hover:flex"
                aria-label={`Unpin ${name}`}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function QuickSettingsPanel() {
  const t = useT();
  const { anim, update } = useAppearance();
  const [volume, setVolume] = useState(50);
  const [mics, setMics] = useState<{ name: string; isDefault: boolean }[]>([]);
  const [mic, setMic] = useState("");

  useEffect(() => {
    invoke<{ name: string; isDefault: boolean }[]>("list_microphones").then(setMics).catch(() => undefined);
    invoke<{ microphone: string }>("get_settings")
      .then((s) => setMic(s.microphone ?? ""))
      .catch(() => undefined);
  }, []);

  const onMic = async (name: string) => {
    setMic(name);
    try {
      const settings = await invoke<Record<string, unknown>>("get_settings");
      await invoke("save_app_settings", { settings: { ...settings, microphone: name } });
    } catch {
      /* ignore */
    }
  };
  const quiet = anim.cfg.noise.quiet;

  return (
    <div className="space-y-4 pt-1">
      <label className="block text-[12px]">
        <span className="mb-2 flex justify-between text-[var(--soul-muted)]">
          <span>{t("qs.volume")}</span>
          <span className="tabular-nums">{volume}%</span>
        </span>
        <input
          type="range"
          min={0}
          max={100}
          value={volume}
          onChange={(e) => {
            const v = Number(e.target.value);
            setVolume(v);
            invoke("island_set_volume", { percent: v }).catch(() => undefined);
          }}
          className="soul-range w-full"
          style={{ ["--fill" as string]: `${volume}%` }}
        />
      </label>
      <label className="block text-[12px]">
        <span className="mb-1.5 block text-[var(--soul-muted)]">{t("qs.mic")}</span>
        <select value={mic} onChange={(e) => onMic(e.target.value)} className="soul-input w-full px-3 py-1.5">
          <option value="">{t("qs.systemDefault")}</option>
          {mics.map((m) => (
            <option key={m.name} value={m.name}>
              {m.name}
            </option>
          ))}
        </select>
      </label>
      <div className="flex items-center justify-between gap-3 text-[12px]">
        <span>
          <span className="block">{t("qs.quiet")}</span>
          <span className="block text-[11px] text-[var(--soul-muted)]">{t("qs.quietHint")}</span>
        </span>
        <MiniSwitch
          checked={quiet}
          onChange={(v) => update({ ...anim.cfg, noise: { ...anim.cfg.noise, quiet: v } })}
        />
      </div>
      <Press
        onClick={() => invoke("open_settings_window").catch(() => undefined)}
        className="text-[11px] text-white/40 hover:text-white/80"
      >
        {t("qs.more")} →
      </Press>
    </div>
  );
}

export function MiniSwitch({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  const anim = useAnim();
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={(e) => {
        e.stopPropagation();
        onChange(!checked);
      }}
      className="relative h-[22px] w-[38px] shrink-0 rounded-full transition-colors"
      style={{ background: checked ? "var(--accent, var(--soul-accent))" : "rgba(255,255,255,0.14)" }}
    >
      <motion.span
        className="absolute top-[3px] h-4 w-4 rounded-full bg-white shadow"
        animate={{ left: checked ? 19 : 3 }}
        transition={anim.spring("bouncy")}
      />
    </button>
  );
}
