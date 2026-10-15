import { FormEvent, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion, useMotionValue, useSpring } from "framer-motion";
import Markdown from "react-markdown";
import { useAnim, useT } from "../anim/AnimationProvider";
import { SpiritOrDot, type SpiritState } from "../spirit/Spirit";
import { tokens } from "../theme/tokens";

type Phase = "loading" | "select" | "listening" | "processing" | "answer" | "error";

interface SessionPayload {
  dataUrl: string;
  width: number;
  height: number;
  phase: string;
}

interface StatusPayload {
  state: string;
  message?: string | null;
}

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface AnswerPayload {
  answer: string;
  history: { role: string; text: string }[];
  selection: Rect;
}

interface Point {
  x: number;
  y: number;
}

const ACCENT = tokens.accentAsk;

export default function AskView() {
  const anim = useAnim();
  const t = useT();
  const [phase, setPhase] = useState<Phase>("loading");
  const [dataUrl, setDataUrl] = useState<string | null>(null);
  const [natural, setNatural] = useState<{ w: number; h: number } | null>(null);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [answer, setAnswer] = useState<string | null>(null);
  const [history, setHistory] = useState<{ role: string; text: string }[]>([]);
  const [selection, setSelection] = useState<Rect | null>(null);
  const [question, setQuestion] = useState("");
  const [copied, setCopied] = useState(false);
  const [settled, setSettled] = useState(0);

  const dragStart = useRef<Point | null>(null);
  const [draft, setDraft] = useState<Rect | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  // Spirit guide follows the cursor while selecting (spring-lagged), then docks at the selection.
  const gx = useMotionValue(-200);
  const gy = useMotionValue(-200);
  const springCfg = anim.spring("gentle");
  const sx = useSpring(gx, "stiffness" in springCfg ? (springCfg as object) : { stiffness: 2000, damping: 100 });
  const sy = useSpring(gy, "stiffness" in springCfg ? (springCfg as object) : { stiffness: 2000, damping: 100 });

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    let alive = true;
    const on = <T,>(ev: string, cb: (p: T) => void) =>
      listen<T>(ev, (e) => cb(e.payload)).then((u) => (alive ? unsubs.push(u) : u()));

    on<SessionPayload>("ask://session", (p) => {
      setDataUrl(p.dataUrl);
      setNatural({ w: p.width, h: p.height });
      setPhase("select");
      setAnswer(null);
      setHistory([]);
      setSelection(null);
      setDraft(null);
      setStatusMsg(null);
    });
    on<StatusPayload>("ask://status", (p) => {
      const st = p.state;
      if (st === "listening") setPhase("listening");
      else if (st === "processing" || st === "retrying") setPhase("processing");
      else if (st === "error") setPhase("error");
      setStatusMsg(p.message ?? null);
    });
    on<AnswerPayload>("ask://answer", (p) => {
      setAnswer(p.answer);
      setHistory(p.history);
      setSelection(p.selection);
      setPhase("answer");
      setStatusMsg(null);
      setQuestion("");
    });
    on("ask://closed", () => {
      setPhase("loading");
      setDataUrl(null);
      setSelection(null);
      setDraft(null);
    });
    // Talk key + mouse drag: the hook streams the drag (image pixel coordinates).
    on<{ phase: "start" | "move" | "cancel"; x: number; y: number }>("ask://drag", (p) => {
      if (p.phase === "start") {
        dragStart.current = { x: p.x, y: p.y };
        setDraft({ x: p.x, y: p.y, width: 0, height: 0 });
      } else if (p.phase === "move" && dragStart.current) {
        const s0 = dragStart.current;
        setDraft({
          x: Math.min(s0.x, p.x),
          y: Math.min(s0.y, p.y),
          width: Math.abs(p.x - s0.x),
          height: Math.abs(p.y - s0.y),
        });
      } else if (p.phase === "cancel") {
        dragStart.current = null;
        setDraft(null);
      }
    });
    on<Rect>("ask://selected", (r) => {
      dragStart.current = null;
      setDraft(null);
      setSelection(r);
      setSettled((n) => n + 1);
      setPhase("listening");
      setStatusMsg(null);
    });

    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") invoke("ask_cancel").catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => {
      alive = false;
      unsubs.forEach((u) => u());
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  // Dock the guide at the selection's top-right corner.
  useEffect(() => {
    const el = containerRef.current;
    if (!selection || !el || !natural) return;
    const r = el.getBoundingClientRect();
    gx.set(((selection.x + selection.width) / natural.w) * r.width - 18);
    gy.set((selection.y / natural.h) * r.height - 46);
  }, [selection, natural, gx, gy]);

  function toLocal(e: React.MouseEvent): Point {
    const el = containerRef.current!;
    const r = el.getBoundingClientRect();
    const nw = natural?.w ?? r.width;
    const nh = natural?.h ?? r.height;
    return {
      x: Math.max(0, Math.round(((e.clientX - r.left) / r.width) * nw)),
      y: Math.max(0, Math.round(((e.clientY - r.top) / r.height) * nh)),
    };
  }

  function onPointerDown(e: React.MouseEvent) {
    if (phase !== "select") return;
    const p = toLocal(e);
    dragStart.current = p;
    setDraft({ x: p.x, y: p.y, width: 0, height: 0 });
  }

  function onPointerMove(e: React.MouseEvent) {
    if (phase === "select" && !selection) {
      gx.set(e.clientX + 18);
      gy.set(e.clientY + 14);
    }
    if (!dragStart.current || phase !== "select") return;
    const p = toLocal(e);
    setDraft({
      x: Math.min(dragStart.current.x, p.x),
      y: Math.min(dragStart.current.y, p.y),
      width: Math.abs(p.x - dragStart.current.x),
      height: Math.abs(p.y - dragStart.current.y),
    });
  }

  async function onPointerUp() {
    if (!dragStart.current || !draft || phase !== "select") return;
    dragStart.current = null;
    if (draft.width < 8 || draft.height < 8) {
      setDraft(null);
      return;
    }
    const region = { ...draft };
    setDraft(null);
    setSelection(region);
    setSettled((n) => n + 1);
    setPhase("listening");
    try {
      await invoke("ask_region_selected", { region });
    } catch (err) {
      setPhase("error");
      setStatusMsg(String(err));
    }
  }

  async function onSubmitText(e: FormEvent) {
    e.preventDefault();
    const text = question.trim();
    if (!text) return;
    setPhase("processing");
    try {
      if (answer) await invoke("ask_followup", { text, withAudio: false });
      else await invoke("ask_submit", { text });
    } catch (err) {
      setPhase("error");
      setStatusMsg(String(err));
    }
  }

  async function onVoiceFollowup() {
    setPhase("listening");
    try {
      await invoke("ask_followup", { text: null, withAudio: true });
    } catch (err) {
      setPhase("error");
      setStatusMsg(String(err));
    }
  }

  async function onCopy() {
    if (!answer) return;
    await navigator.clipboard.writeText(answer);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  const spiritState: SpiritState =
    phase === "listening"
      ? "listening"
      : phase === "processing"
        ? "thinking"
        : phase === "error"
          ? "error"
          : phase === "answer"
            ? "success"
            : "idle";
  const fx = anim.on("askEffects");
  const rect = draft ?? selection;
  const pct = (r: Rect) => {
    const nw = natural?.w || 1;
    const nh = natural?.h || 1;
    return {
      left: `${(r.x / nw) * 100}%`,
      top: `${(r.y / nh) * 100}%`,
      width: `${(r.width / nw) * 100}%`,
      height: `${(r.height / nh) * 100}%`,
    };
  };

  const chip =
    phase === "select"
      ? t("ask.select")
      : phase === "listening"
        ? statusMsg || t("ask.listening")
        : phase === "processing"
          ? statusMsg || t("ask.thinking")
          : phase === "error"
            ? statusMsg || t("island.error")
            : phase === "answer"
              ? t("ask.answer")
              : t("ask.starting");

  return (
    <div
      ref={containerRef}
      className="relative h-screen w-screen overflow-hidden bg-black select-none"
      onMouseDown={onPointerDown}
      onMouseMove={onPointerMove}
      onMouseUp={onPointerUp}
      style={{ cursor: phase === "select" ? "crosshair" : "default" }}
    >
      {dataUrl && (
        <img
          src={dataUrl}
          alt=""
          className="pointer-events-none absolute inset-0 h-full w-full object-fill"
          draggable={false}
          onLoad={(e) =>
            !natural && setNatural({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })
          }
        />
      )}

      {/* Dim: whole screen until there is a region, then a cut-out around it. */}
      <AnimatePresence>
        {dataUrl && !rect && (
          <motion.div
            key="dim"
            className="pointer-events-none absolute inset-0 bg-black/55"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: anim.dur(220) }}
          />
        )}
      </AnimatePresence>
      {rect && (
        <motion.div
          key={`ring-${settled}`}
          className="pointer-events-none absolute rounded-[14px]"
          initial={draft ? false : { scale: fx ? 1.04 : 1 }}
          animate={{ ...pct(rect), scale: 1 }}
          transition={draft ? { duration: 0 } : anim.spring("bouncy")}
          style={{
            boxShadow: `0 0 0 9999px rgba(0,0,0,0.55), 0 0 0 2px ${ACCENT}${fx ? `, 0 0 28px 2px ${ACCENT}66` : ""}`,
          }}
        >
          {fx && !draft && (
            <motion.span
              className="absolute -inset-1.5 rounded-[18px] border-2"
              style={{ borderColor: ACCENT }}
              initial={{ opacity: 0.9, scale: 1 }}
              animate={{ opacity: 0, scale: 1.08 }}
              transition={{ duration: anim.dur(700), ease: "easeOut" }}
            />
          )}
        </motion.div>
      )}

      {/* Spirit guide */}
      {dataUrl && anim.on("spirit") && anim.cfg.noise.expandOnAsk && (
        <motion.div className="pointer-events-none absolute left-0 top-0 z-30" style={{ x: sx, y: sy }}>
          <SpiritOrDot state={spiritState} color={phase === "error" ? tokens.danger : ACCENT} size={26} />
        </motion.div>
      )}

      {/* Status chip */}
      <motion.div
        layout
        transition={anim.spring("morph")}
        className="island-glass pointer-events-none absolute left-1/2 top-4 z-20 flex -translate-x-1/2 items-center gap-2.5 rounded-full px-4 py-2 text-[13px] text-white"
      >
        <span className="h-2 w-2 rounded-full" style={{ background: phase === "error" ? tokens.danger : ACCENT, boxShadow: `0 0 8px ${ACCENT}` }} />
        <span className="max-w-[60vw] truncate">{chip}</span>
        <span className="text-[11px] text-white/40">{t("ask.escHint")}</span>
      </motion.div>

      {/* Question input near the selection (before the first answer). */}
      <AnimatePresence>
        {(phase === "listening" || phase === "processing") && selection && !answer && (
          <motion.form
            key="q"
            onSubmit={onSubmitText}
            initial={{ opacity: 0, y: -6, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -4 }}
            transition={anim.spring("ui")}
            className="island-glass absolute z-30 flex w-[min(420px,80vw)] gap-2 rounded-2xl p-2"
            style={inputStyle(selection, natural)}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <input
              autoFocus
              className="soul-input min-w-0 flex-1 px-3 py-2 text-sm"
              style={{ ["--accent" as string]: ACCENT }}
              placeholder={t("ask.typeQuestion")}
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              disabled={phase === "processing"}
            />
            <button
              type="submit"
              className="rounded-xl px-3 py-2 text-sm font-semibold text-[#120b1f] disabled:opacity-40"
              style={{ background: ACCENT }}
              disabled={phase === "processing" || !question.trim()}
            >
              {t("ask.send")}
            </button>
          </motion.form>
        )}
      </AnimatePresence>

      {/* Answer card with follow-up thread */}
      <AnimatePresence>
        {answer && selection && (
          <motion.div
            key="answer"
            initial={{ opacity: 0, scale: 0.92, y: 8 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96 }}
            transition={anim.spring("morph")}
            className="island-glass absolute z-40 flex max-h-[56vh] w-[min(440px,90vw)] flex-col rounded-[24px]"
            style={{ ...cardStyle(selection, natural), ["--accent" as string]: ACCENT }}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <div className="flex items-center gap-2 px-4 pb-2 pt-3">
              <span className="text-[13px] font-semibold">Soul</span>
              <span className="rounded-full px-1.5 py-px text-[9.5px] font-medium uppercase tracking-wider" style={{ color: ACCENT, background: "rgb(255 255 255 / 0.06)" }}>
                {t("island.ask")}
              </span>
              <span className="flex-1" />
              <button type="button" onClick={onCopy} className="soul-btn-ghost rounded-full px-2.5 py-1 text-[11px]">
                {copied ? t("ask.copied") : t("ask.copy")}
              </button>
              <button type="button" onClick={() => invoke("ask_cancel")} className="soul-btn-ghost rounded-full px-2.5 py-1 text-[11px]">
                {t("ask.close")}
              </button>
            </div>
            <div className="soul-scroll ask-md min-h-0 flex-1 overflow-y-auto px-4 pb-3 text-[13.5px] leading-relaxed text-white/90 select-text">
              {history.length > 2 && (
                <div className="mb-3 space-y-2 border-b border-white/10 pb-3 text-[12px]">
                  {history.slice(0, -2).map((turn, i) => (
                    <div key={i} className={turn.role === "user" ? "text-right" : ""}>
                      <span
                        className={`inline-block max-w-[85%] rounded-2xl px-3 py-1.5 text-left ${
                          turn.role === "user" ? "bg-white/10 text-white/85" : "bg-white/4 text-white/60"
                        }`}
                      >
                        {turn.text.slice(0, 220)}
                        {turn.text.length > 220 ? "…" : ""}
                      </span>
                    </div>
                  ))}
                </div>
              )}
              <AnimatePresence mode="wait">
                {phase === "processing" ? (
                  <motion.div key="thinking" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="py-2 text-white/50">
                    {t("ask.thinking")}
                  </motion.div>
                ) : (
                  <motion.div key={answer} initial={{ opacity: 0, y: 4 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: anim.dur(220) }}>
                    <Markdown>{answer}</Markdown>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
            <form onSubmit={onSubmitText} className="flex gap-2 border-t border-white/8 p-2.5">
              <input
                className="soul-input min-w-0 flex-1 px-3 py-2 text-sm"
                placeholder={t("ask.followup")}
                value={question}
                onChange={(e) => setQuestion(e.target.value)}
                disabled={phase === "processing"}
              />
              <button
                type="button"
                onClick={onVoiceFollowup}
                disabled={phase === "processing" || phase === "listening"}
                className="soul-btn-ghost rounded-xl px-3 text-sm disabled:opacity-40"
                aria-label={t("ask.voice")}
                title={t("ask.voice")}
              >
                <MicIcon />
              </button>
              <button
                type="submit"
                className="rounded-xl px-3 py-2 text-sm font-semibold text-[#120b1f] disabled:opacity-40"
                style={{ background: ACCENT }}
                disabled={phase === "processing" || !question.trim()}
              >
                {t("ask.send")}
              </button>
            </form>
          </motion.div>
        )}
      </AnimatePresence>

      <AnimatePresence>
        {phase === "error" && statusMsg && (
          <motion.div
            key="err"
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            transition={anim.spring("ui")}
            className="island-glass absolute bottom-6 left-1/2 z-30 flex max-w-lg -translate-x-1/2 items-center gap-3 rounded-2xl px-4 py-3 text-sm"
            style={{ color: tokens.danger }}
          >
            <span className="min-w-0 flex-1">{statusMsg}</span>
            <button type="button" className="soul-btn-ghost rounded-full px-3 py-1 text-xs text-white" onClick={() => invoke("ask_cancel")}>
              {t("ask.close")}
            </button>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function MicIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
    </svg>
  );
}

function inputStyle(sel: Rect, natural: { w: number; h: number } | null) {
  const nw = natural?.w || 1;
  const nh = natural?.h || 1;
  const below = (sel.y + sel.height) / nh < 0.82;
  return {
    left: `${Math.min(70, (sel.x / nw) * 100)}%`,
    top: below ? `calc(${((sel.y + sel.height) / nh) * 100}% + 12px)` : `calc(${(sel.y / nh) * 100}% - 64px)`,
  };
}

/** Dock the answer card beside the selection (right, then left), else below / above it;
 * always clamped inside the viewport. Works in CSS px. */
function cardStyle(sel: Rect, natural: { w: number; h: number } | null) {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const kx = vw / (natural?.w || vw);
  const ky = vh / (natural?.h || vh);
  const L = sel.x * kx;
  const T = sel.y * ky;
  const R = (sel.x + sel.width) * kx;
  const B = (sel.y + sel.height) * ky;
  const W = Math.min(440, vw * 0.9);
  const H = vh * 0.56;
  const gap = 14;
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
  let left: number;
  let top: number;
  if (vw - R >= W + gap * 2) {
    left = R + gap;
    top = T;
  } else if (L >= W + gap * 2) {
    left = L - gap - W;
    top = T;
  } else {
    left = R - W;
    top = vh - B >= 200 ? B + gap : T - H - gap;
  }
  return {
    left: clamp(left, gap, vw - W - gap),
    top: clamp(top, 64, Math.max(64, vh - H - gap)),
  };
}
