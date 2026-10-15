import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";
import { tr } from "../i18n";
import { Keycap } from "./ui";

/**
 * Click "Record", press a key. `single` captures one key or mouse button for the Smart talk key
 * (names match platform::hook::TalkKey); `combo` captures modifier+key for global shortcuts
 * ("Ctrl+Shift+Space"). All Soul hotkeys are paused while recording so the key isn't consumed.
 */

const SINGLE_KEYS = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
  "ScrollLock",
  "Pause",
  "Insert",
  "ContextMenu",
]);

const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

const LABELS: Record<string, string> = {
  ControlLeft: "Ctrl (L)",
  ControlRight: "Ctrl (P)",
  ShiftLeft: "Shift (L)",
  ShiftRight: "Shift (P)",
  AltLeft: "Alt (L)",
  MetaLeft: "Win (L)",
  MetaRight: "Win (P)",
  CapsLock: "Caps Lock",
  ScrollLock: "Scroll Lock",
  Pause: "Pause",
  Insert: "Insert",
  ContextMenu: "Menu key",
  MouseMiddle: "Mouse ⊙",
  Mouse4: "Mouse ◀ (side)",
  Mouse5: "Mouse ▶ (side)",
};

export function talkKeyLabel(code: string): string {
  return LABELS[code] ?? code;
}

function isFunctionKey(code: string): boolean {
  const m = code.match(/^F(\d{1,2})$/);
  return Boolean(m && Number(m[1]) >= 1 && Number(m[1]) <= 24);
}

/** KeyboardEvent.code → key name accepted by parse_hotkey in lib.rs (or null). */
function comboKey(code: string): string | null {
  if (code === "Space") return "Space";
  if (code === "Enter" || code === "NumpadEnter") return "Enter";
  if (code === "Tab") return "Tab";
  const letter = code.match(/^Key([A-Z])$/);
  if (letter) return letter[1];
  const digit = code.match(/^Digit(\d)$/);
  if (digit) return digit[1];
  return null;
}

interface Props {
  value: string;
  onChange: (value: string) => void;
  mode: "single" | "combo";
  placeholder?: string;
}

export default function HotkeyRecorder({ value, onChange, mode, placeholder }: Props) {
  const anim = useAnim();
  const lang = anim.cfg.uiLang;
  const [recording, setRecording] = useState(false);
  const langRef = useRef(lang);
  langRef.current = lang;
  const [hint, setHint] = useState<string | null>(null);
  const pressed = useRef<string[]>([]);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    if (!recording) return;
    pressed.current = [];
    invoke("set_hotkeys_suspended", { suspended: true }).catch(() => undefined);

    const finish = (result: string | null, message: string | null = null) => {
      if (result) onChangeRef.current(result);
      setHint(message);
      setRecording(false);
    };

    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      if (e.code === "Escape" && pressed.current.length === 0) {
        finish(null);
        return;
      }
      if (!pressed.current.includes(e.code)) pressed.current.push(e.code);
      if (mode === "combo" && !MODIFIER_CODES.has(e.code)) {
        const key = comboKey(e.code);
        if (!key) {
          finish(null, tr(langRef.current, `${e.code} nie jest obsługiwany — użyj litery, cyfry, Spacji, Entera lub Taba.`, `${e.code} isn't supported — use a letter, digit, Space, Enter or Tab.`));
          return;
        }
        const mods = [
          e.ctrlKey && "Ctrl",
          e.metaKey && "Super",
          e.altKey && "Alt",
          e.shiftKey && "Shift",
        ].filter(Boolean);
        if (mods.length === 0) {
          finish(null, tr(langRef.current, "Dodaj co najmniej jeden modyfikator (Ctrl, Win, Alt lub Shift).", "Add at least one modifier (Ctrl, Win, Alt or Shift)."));
          return;
        }
        finish([...mods, key].join("+"));
      }
    };

    // Single keys are taken on release so AltGr (Ctrl+Right Alt) can be recognized and refused.
    const onKeyUp = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (mode !== "single") return;
      const codes = pressed.current;
      if (codes.length === 0) return;
      if (codes.includes("AltRight")) {
        finish(null, tr(langRef.current, "Prawy Alt to AltGr na polskim układzie (ą, ć, ę…) — wybierz inny klawisz.", "Right Alt is AltGr on Polish layouts (ą, ć, ę…) — pick another key."));
        return;
      }
      const code = codes[0];
      if (SINGLE_KEYS.has(code) || isFunctionKey(code)) {
        finish(code);
      } else {
        finish(
          null,
          tr(
            langRef.current,
            `${code} zepsułby pisanie. Użyj Ctrl/Shift/Win/lewego Alta, Caps Lock, F1–F24, Pause, Insert, Menu lub bocznego przycisku myszy.`,
            `${code} would break typing. Use Ctrl/Shift/Win/Left Alt, Caps Lock, F1–F24, Pause, Insert, Menu or a mouse side button.`,
          ),
        );
      }
    };

    const onMouseDown = (e: MouseEvent) => {
      if (mode !== "single") return;
      const name = e.button === 1 ? "MouseMiddle" : e.button === 3 ? "Mouse4" : e.button === 4 ? "Mouse5" : null;
      if (!name) return;
      e.preventDefault();
      e.stopPropagation();
      finish(name);
    };
    // Side buttons would otherwise navigate the Settings webview back/forward.
    const onMouseUp = (e: MouseEvent) => {
      if (e.button === 3 || e.button === 4) e.preventDefault();
    };

    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("mouseup", onMouseUp, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("mousedown", onMouseDown, true);
      window.removeEventListener("mouseup", onMouseUp, true);
      invoke("set_hotkeys_suspended", { suspended: false }).catch(() => undefined);
    };
  }, [recording, mode]);

  const keys = mode === "single" ? [talkKeyLabel(value)] : (value || placeholder || "").split("+").filter(Boolean);

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <motion.div
          className="relative flex min-h-[40px] min-w-[220px] flex-1 items-center gap-1.5 rounded-[14px] border px-2.5 py-1.5"
          animate={{
            borderColor: recording ? "var(--soul-accent)" : "rgba(255,255,255,0.10)",
            boxShadow: recording ? "0 0 0 4px rgb(var(--soul-accent-rgb) / 0.15)" : "0 0 0 0px rgb(var(--soul-accent-rgb) / 0)",
          }}
          transition={anim.spring("ui")}
          style={{ background: "rgba(255,255,255,0.04)" }}
        >
          {recording ? (
            <span className="flex items-center gap-2 text-[12.5px] text-[var(--soul-accent)]">
              <span className="soul-dot-pulse-wrap relative inline-flex h-2 w-2">
                <span className="soul-dot-pulse absolute inset-0 rounded-full bg-[var(--soul-accent)]" />
                <span className="relative h-2 w-2 rounded-full bg-[var(--soul-accent)]" />
              </span>
              {mode === "single"
                ? tr(lang, "Naciśnij klawisz lub przycisk myszy… (Esc anuluje)", "Press a key or mouse button… (Esc cancels)")
                : tr(lang, "Naciśnij kombinację… (Esc anuluje)", "Press the combination… (Esc cancels)")}
            </span>
          ) : (
            keys.map((k, i) => (
              <span key={`${k}-${i}`} className="flex items-center gap-1.5">
                {i > 0 && <span className="text-[11px] text-white/30">+</span>}
                <Keycap>{k}</Keycap>
              </span>
            ))
          )}
        </motion.div>
        <button
          type="button"
          onClick={() => {
            setHint(null);
            setRecording((r) => !r);
          }}
          className={`shrink-0 rounded-full px-3.5 py-2 text-[12px] transition-colors ${
            recording ? "bg-white/10 text-white" : "bg-[var(--soul-accent)]/15 text-[var(--soul-accent)] hover:bg-[var(--soul-accent)]/25"
          }`}
        >
          {recording ? tr(lang, "Anuluj", "Cancel") : tr(lang, "Nagraj", "Record")}
        </button>
      </div>
      {hint && <span className="text-[11.5px] text-amber-300">{hint}</span>}
    </div>
  );
}
