import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { tr, type UiLang } from "../i18n";
import { clockOf, SPEAKER_COLORS, useElapsed, useNotes, type Note } from "../lib/notes";

/** Call notes: start / stop, live transcript, summary + to-dos, transcript with speaker renaming,
 *  Markdown copy / export. Shared by the Island "Calls" tab (`island`) and Settings. */
export default function NotesView({ lang, island = false }: { lang: UiLang; island?: boolean }) {
  const notes = useNotes();
  const [openId, setOpenId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const run = (cmd: string, args?: Record<string, unknown>) =>
    invoke(cmd, args)
      .then(() => setError(null))
      .catch((e) => setError(String(e)));
  const open = notes.notes.find((n) => n.id === openId) ?? null;
  const rec = notes.recordingNote;
  const elapsed = useElapsed(rec ? rec.created : null);
  // The Island window is non-activating; typing needs it to take the keyboard.
  const focus = (focused: boolean) => island && invoke("island_focus_window", { focused }).catch(() => undefined);

  return (
    <div className="flex flex-col gap-3 text-[12px]" onClick={(e) => e.stopPropagation()}>
      <div className="flex items-center gap-2">
        {rec ? (
          <>
            <RecDot />
            <span className="font-semibold tabular-nums text-[#ff6b6b]">REC {clockOf(elapsed)}</span>
            <span className="min-w-0 flex-1 truncate text-white/55">{rec.app ?? ""}</span>
            <button type="button" className="soul-btn-ghost rounded-full px-3 py-1 text-[11.5px]" onClick={() => run("notes_stop")}>
              {tr(lang, "Zatrzymaj", "Stop")}
            </button>
          </>
        ) : (
          <>
            <span className="min-w-0 flex-1 truncate text-white/55">
              {tr(lang, "Nagrywa mikrofon i dźwięk komputera, transkrybuje lokalnie.", "Records your mic and computer audio, transcribed locally.")}
            </span>
            <button
              type="button"
              className="rounded-full bg-[#ff5a5a] px-3 py-1 text-[11.5px] font-semibold text-[#1a0b0d]"
              onClick={() => run("notes_start")}
            >
              ● {tr(lang, "Nagraj rozmowę", "Record a call")}
            </button>
          </>
        )}
      </div>
      {error && <div className="text-[11.5px] text-[var(--soul-danger)]">{error}</div>}

      {rec && !open && (
        <div className="flex flex-col gap-1 rounded-xl bg-white/[0.04] p-2.5">
          {rec.lines.length === 0 && <span className="text-white/45">{tr(lang, "Słucham…", "Listening…")}</span>}
          {rec.lines.slice(-6).map((l) => (
            <LineRow key={l.id} note={rec} line={l} />
          ))}
        </div>
      )}

      {open ? (
        <NoteDetail
          note={open}
          lang={lang}
          working={notes.working.includes(open.id)}
          onBack={() => setOpenId(null)}
          run={run}
          focus={focus}
        />
      ) : (
        <div className="flex flex-col gap-1">
          {notes.notes.length === 0 && (
            <span className="text-white/40">{tr(lang, "Brak nagranych rozmów.", "No recorded calls yet.")}</span>
          )}
          {notes.notes.map((n) => (
            <button
              key={n.id}
              type="button"
              onClick={() => setOpenId(n.id)}
              className="flex items-center gap-2 rounded-lg px-2 py-1.5 text-left hover:bg-white/[0.06]"
            >
              <span className="min-w-0 flex-1 truncate text-white/90">{n.title}</span>
              <span className="shrink-0 text-[11px] text-white/45">
                {statusText(n, notes.working.includes(n.id), lang) ?? `${new Date(n.created).toLocaleDateString()} · ${clockOf(n.duration)}`}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function statusText(n: Note, working: boolean, lang: UiLang): string | null {
  if (n.status === "recording") return tr(lang, "nagrywanie…", "recording…");
  if (n.status === "processing") return tr(lang, "transkrypcja…", "transcribing…");
  if (n.status === "failed") return tr(lang, "błąd", "failed");
  if (working) return tr(lang, "podsumowuję…", "summarizing…");
  return null;
}

function RecDot() {
  return <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-[#ff4d4d] shadow-[0_0_8px_#ff4d4d]" />;
}

/** Pulsing `● REC 4:12` for the collapsed / peek Island pill. */
export function RecBadge({ startedMs }: { startedMs: number }) {
  const elapsed = useElapsed(startedMs);
  return (
    <span className="inline-flex items-center gap-1.5 text-[12px] font-semibold tabular-nums text-[#ff6b6b]">
      <RecDot />
      REC {clockOf(elapsed)}
    </span>
  );
}

function speakerOf(note: Note, id: string) {
  return note.speakers.find((s) => s.id === id);
}

function LineRow({ note, line }: { note: Note; line: Note["lines"][number] }) {
  const s = speakerOf(note, line.speaker);
  return (
    <div className="leading-relaxed">
      <span className="font-semibold" style={{ color: SPEAKER_COLORS[(s?.color ?? 0) % SPEAKER_COLORS.length] }}>
        {s?.name ?? line.speaker}
      </span>{" "}
      <span className="text-[10.5px] tabular-nums text-white/35">{clockOf(line.start)}</span>{" "}
      <span className="text-white/85">{line.text}</span>
    </div>
  );
}

function NoteDetail({
  note,
  lang,
  working,
  onBack,
  run,
  focus,
}: {
  note: Note;
  lang: UiLang;
  working: boolean;
  onBack: () => void;
  run: (cmd: string, args?: Record<string, unknown>) => Promise<void>;
  focus: (focused: boolean) => unknown;
}) {
  const [view, setView] = useState<"summary" | "transcript">(note.summary ? "summary" : "transcript");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const busy = working || note.status === "processing" || note.status === "recording";
  const status = statusText(note, working, lang);
  const btn = "soul-btn-ghost rounded-full px-3 py-1 text-[11.5px] disabled:opacity-40";
  const copy = () =>
    invoke<string | null>("notes_markdown", { id: note.id })
      .then((md) => navigator.clipboard.writeText(md ?? ""))
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => undefined);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <button type="button" className="text-white/55 hover:text-white" onClick={onBack} aria-label={tr(lang, "Wstecz", "Back")}>
          ←
        </button>
        <input
          key={note.title}
          defaultValue={note.title}
          className="min-w-0 flex-1 rounded-md bg-transparent px-1 text-[14px] font-semibold text-white outline-none focus:bg-white/[0.06]"
          onFocus={() => focus(true)}
          onBlur={(e) => {
            focus(false);
            const title = e.target.value.trim();
            if (title && title !== note.title) void run("notes_rename", { id: note.id, title });
          }}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
      </div>
      <div className="text-[11px] text-white/45">
        {new Date(note.created).toLocaleString()} · {clockOf(note.duration)}
        {note.app ? ` · ${note.app}` : ""}
        {status ? ` · ${status}` : ""}
      </div>
      {note.error && (
        <div className="flex items-center gap-2 text-[11.5px] text-[var(--soul-danger)]">
          <span className="min-w-0 flex-1">{note.error}</span>
          <button type="button" className={btn} disabled={busy} onClick={() => run("notes_retry", { id: note.id })}>
            {tr(lang, "Ponów", "Retry")}
          </button>
        </div>
      )}

      <div className="flex gap-0.5 self-start rounded-full bg-white/5 p-0.5" role="tablist">
        {(["summary", "transcript"] as const).map((v) => (
          <button
            key={v}
            type="button"
            role="tab"
            aria-selected={view === v}
            onClick={() => setView(v)}
            className={`rounded-full px-3 py-1 text-[11px] ${view === v ? "bg-white/12 text-white" : "text-white/50 hover:text-white/80"}`}
          >
            {v === "summary" ? tr(lang, "Podsumowanie", "Summary") : tr(lang, "Transkrypcja", "Transcript")}
          </button>
        ))}
      </div>

      {view === "summary" ? (
        <div className="flex flex-col gap-3">
          {note.summary ? (
            <p className="leading-relaxed text-white/85">{note.summary}</p>
          ) : (
            <span className="text-white/45">
              {busy
                ? tr(lang, "Notatki będą gotowe po transkrypcji.", "Notes will be ready after transcription.")
                : tr(lang, "Brak podsumowania (potrzebny klucz Gemini).", "No summary yet (needs a Gemini key).")}
            </span>
          )}
          {note.keyPoints.length > 0 && (
            <div>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-white/45">
                {tr(lang, "Kluczowe punkty", "Key points")}
              </div>
              <ul className="list-disc pl-4 text-white/85">
                {note.keyPoints.map((p, i) => (
                  <li key={i}>{p}</li>
                ))}
              </ul>
            </div>
          )}
          {note.todos.length > 0 && (
            <div>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-white/45">
                {tr(lang, "Do zrobienia", "To do")}
              </div>
              {note.todos.map((t) => (
                <label key={t.id} className="flex cursor-pointer items-start gap-2 py-0.5">
                  <input
                    type="checkbox"
                    className="mt-0.5 accent-[var(--soul-accent)]"
                    checked={t.done}
                    onChange={() => run("notes_toggle_todo", { id: note.id, todo: t.id })}
                  />
                  <span className={t.done ? "text-white/40 line-through" : "text-white/85"}>
                    {t.text}
                    {t.owner && <span className="text-white/45"> — {t.owner}</span>}
                    {t.due && <span className="text-white/45"> ({t.due})</span>}
                  </span>
                </label>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap gap-1">
            {note.speakers.map((s) =>
              renaming === s.id ? (
                <input
                  key={s.id}
                  autoFocus
                  defaultValue={s.name}
                  className="w-28 rounded-full bg-white/10 px-2 py-0.5 text-[11px] text-white outline-none"
                  onFocus={() => focus(true)}
                  onBlur={(e) => {
                    focus(false);
                    setRenaming(null);
                    const name = e.target.value.trim();
                    if (name && name !== s.name) void run("notes_rename_speaker", { id: note.id, speaker: s.id, name });
                  }}
                  onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
                />
              ) : (
                <button
                  key={s.id}
                  type="button"
                  title={tr(lang, "Kliknij, aby zmienić nazwę", "Click to rename")}
                  onClick={() => setRenaming(s.id)}
                  className="rounded-full border border-white/10 px-2 py-0.5 text-[11px]"
                  style={{ color: SPEAKER_COLORS[s.color % SPEAKER_COLORS.length] }}
                >
                  {s.name}
                </button>
              ),
            )}
          </div>
          {note.lines.length === 0 && <span className="text-white/45">{tr(lang, "Brak transkrypcji.", "No transcript.")}</span>}
          {note.lines.map((l) => (
            <LineRow key={l.id} note={note} line={l} />
          ))}
        </div>
      )}

      <div className="flex flex-wrap gap-1.5">
        <button type="button" className={btn} onClick={copy}>
          {copied ? tr(lang, "Skopiowano", "Copied") : tr(lang, "Kopiuj Markdown", "Copy Markdown")}
        </button>
        <button type="button" className={btn} onClick={() => run("notes_export", { id: note.id })}>
          {tr(lang, "Eksportuj .md", "Export .md")}
        </button>
        <button
          type="button"
          className={btn}
          disabled={busy || note.lines.length === 0}
          onClick={() => run("notes_summarize", { id: note.id })}
        >
          {tr(lang, "Podsumuj ponownie", "Regenerate summary")}
        </button>
        <button
          type="button"
          className={`${btn} text-[var(--soul-danger)]`}
          disabled={busy}
          onClick={() => (confirmDelete ? run("notes_delete", { id: note.id }).then(onBack) : setConfirmDelete(true))}
          onBlur={() => setConfirmDelete(false)}
        >
          {confirmDelete ? tr(lang, "Na pewno usunąć?", "Really delete?") : tr(lang, "Usuń", "Delete")}
        </button>
      </div>
    </div>
  );
}
