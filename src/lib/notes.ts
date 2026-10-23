import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Call notes (Rust `notes/`): mirrors `builder::Note` and `NotesState`. */
export interface NoteSpeaker {
  id: string;
  name: string;
  color: number;
  isMe: boolean;
}
export interface NoteLine {
  id: string;
  speaker: string;
  start: number;
  end: number;
  text: string;
}
export interface NoteTodo {
  id: string;
  text: string;
  owner: string | null;
  due: string | null;
  done: boolean;
}
export interface Note {
  id: string;
  title: string;
  titleEdited: boolean;
  created: number;
  duration: number;
  app: string | null;
  status: "recording" | "processing" | "ready" | "failed";
  speakers: NoteSpeaker[];
  lines: NoteLine[];
  summary: string | null;
  keyPoints: string[];
  todos: NoteTodo[];
  error: string | null;
}
export interface NotesState {
  notes: Note[];
  recording: string | null;
  working: string[];
}

export const SPEAKER_COLORS = ["#7dd3fc", "#fbbf24", "#a78bfa", "#34d399", "#f472b6", "#fb923c", "#60a5fa"];

export function clockOf(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** Notes state, refreshed on every `soul://notes-changed` / `island://note-recording`. */
export function useNotes() {
  const [state, setState] = useState<NotesState>({ notes: [], recording: null, working: [] });
  const refresh = useCallback(() => {
    invoke<NotesState>("notes_state").then(setState).catch(() => undefined);
  }, []);
  useEffect(() => {
    let alive = true;
    const unsubs: Array<() => void> = [];
    refresh();
    for (const ev of ["soul://notes-changed", "island://note-recording"]) {
      listen(ev, refresh).then((u) => (alive ? unsubs.push(u) : u()));
    }
    return () => {
      alive = false;
      unsubs.forEach((u) => u());
    };
  }, [refresh]);
  const recordingNote = state.notes.find((n) => n.id === state.recording) ?? null;
  return { ...state, recordingNote, refresh };
}

/** Seconds since `startedMs`, ticking once a second while `startedMs` is set. */
export function useElapsed(startedMs: number | null): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (startedMs === null) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [startedMs]);
  return startedMs === null ? 0 : (now - startedMs) / 1000;
}
