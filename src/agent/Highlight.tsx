import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";

/**
 * Soul Agent highlight: a ring flashed around the element the agent is about to click. The Rust
 * side sizes / positions this click-through, never-focused window around the target
 * (`agent::computer::show_highlight`) and emits `agent://highlight` for every flash.
 */
export default function Highlight() {
  const anim = useAnim();
  const [flash, setFlash] = useState(0);

  useEffect(() => {
    document.documentElement.classList.add("island-root");
    let alive = true;
    let unlisten: (() => void) | undefined;
    listen("agent://highlight", () => setFlash((n) => n + 1)).then((u) => (alive ? (unlisten = u) : u()));
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const animated = anim.level !== "off";
  return (
    <motion.div
      key={flash}
      initial={animated ? { opacity: 0, scale: 1.18 } : false}
      animate={{ opacity: [0, 1, 1, 0.85], scale: 1 }}
      transition={{ duration: anim.dur(420) || 0.01, ease: "easeOut" }}
      style={{
        position: "fixed",
        inset: 3,
        borderRadius: 10,
        border: "3px solid var(--accent, var(--soul-accent, #7c5cff))",
        boxShadow: "0 0 18px 2px var(--accent, var(--soul-accent, #7c5cff)), inset 0 0 12px var(--accent, var(--soul-accent, #7c5cff))",
        pointerEvents: "none",
      }}
    />
  );
}
