import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { motion } from "framer-motion";
import { useAnim, useT } from "../anim/AnimationProvider";
import { SpiritOrDot, type SpiritState } from "../spirit/Spirit";
import { tokens } from "../theme/tokens";

/** Compact status overlay — only used when the Island is disabled. */
type OverlayState =
  | "listening"
  | "processing"
  | "thinking"
  | "retrying"
  | "confirm"
  | "speaking"
  | "done"
  | "error"
  | "idle";

interface OverlayPayload {
  state: OverlayState;
  message?: string | null;
}

const SPIRIT: Record<OverlayState, SpiritState> = {
  idle: "idle",
  listening: "listening",
  processing: "thinking",
  thinking: "thinking",
  retrying: "thinking",
  confirm: "listening",
  speaking: "speaking",
  done: "success",
  error: "error",
};

export default function Overlay() {
  const anim = useAnim();
  const t = useT();
  const [state, setState] = useState<OverlayState>("idle");
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<OverlayPayload>("overlay://state", (event) => {
      setState(event.payload.state);
      setMessage(event.payload.message ?? null);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  const label: Record<OverlayState, string> = {
    idle: "Soul",
    listening: t("island.listening"),
    processing: t("island.processing"),
    thinking: t("island.thinking"),
    retrying: t("island.retrying"),
    confirm: t("island.confirmTitle"),
    speaking: t("island.speaking"),
    done: t("island.done"),
    error: t("island.error"),
  };
  const color = state === "error" ? tokens.danger : state === "done" ? tokens.success : anim.cfg.accent;

  return (
    <div className="flex h-full w-full items-center justify-center bg-transparent p-2">
      <motion.div
        layout
        transition={anim.spring("morph")}
        className="island-glass flex max-w-full items-center gap-3 rounded-[22px] px-3 py-2"
        title={message ?? undefined}
      >
        <SpiritOrDot state={SPIRIT[state]} color={color} size={20} />
        <div className="min-w-0">
          <div className="truncate text-sm font-medium tracking-wide text-white">{label[state]}</div>
          {message && (
            <div
              className={`mt-0.5 text-xs text-[var(--soul-muted)] ${
                state === "error"
                  ? "max-h-16 max-w-[340px] overflow-y-auto whitespace-pre-wrap break-words"
                  : state === "speaking"
                    ? "line-clamp-3 max-w-[340px] break-words"
                    : "max-w-[260px] truncate"
              }`}
            >
              {message}
            </div>
          )}
          {state === "confirm" && (
            <div className="mt-2 flex gap-2">
              <button
                type="button"
                className="rounded-full px-3 py-1 text-xs font-semibold text-[#1a0b0d]"
                style={{ background: tokens.danger }}
                onClick={() => invoke("agent_confirm", { accepted: true }).catch(() => undefined)}
              >
                {t("island.confirm")}
              </button>
              <button
                type="button"
                className="soul-btn-ghost rounded-full px-3 py-1 text-xs"
                onClick={() => invoke("agent_confirm", { accepted: false }).catch(() => undefined)}
              >
                {t("island.cancel")}
              </button>
            </div>
          )}
        </div>
      </motion.div>
    </div>
  );
}
