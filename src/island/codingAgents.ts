/**
 * Coding agent sessions on the Island (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI…):
 * `island://coding-hook` payloads (normalized by `soul --agent-hook`, see `coding_agents/relay.rs`)
 * → per-session state, step ticker, Live Diffs and the one pending approval / question card.
 * Effects (`ack` / `decline`) are returned, not invoked, so this stays testable.
 *
 * Pure (only `./diff.ts`) so `tests/ui/codingAgents.test.ts` can run it under plain Node.
 */
import { buildFileDiff, toOneLine, type FileDiff } from "./diff.ts";

export type AgentId = "claude" | "codex" | "cursor" | "gemini" | "copilot" | "antigravity" | "muse" | "claude-desktop";
export type SessionState = "idle" | "working" | "thinking" | "approval" | "question" | "finished" | "error" | "ratelimit";
export type StepKind = "read" | "edit" | "write" | "run" | "search" | "web" | "other";

export const AGENTS: Record<AgentId, { name: string; color: string }> = {
  claude: { name: "Claude Code", color: "#D97757" },
  "claude-desktop": { name: "Claude", color: "#D97757" },
  codex: { name: "Codex", color: "#10A37F" },
  cursor: { name: "Cursor", color: "#C9CDD4" },
  gemini: { name: "Gemini CLI", color: "#4C8DF6" },
  copilot: { name: "Copilot CLI", color: "#A371F7" },
  antigravity: { name: "Antigravity", color: "#34A853" },
  muse: { name: "Muse", color: "#F778BA" },
};

/** Agents whose permission prompts can be answered on the Island (mirrors `relay::takes_decisions`). */
const DECIDES: AgentId[] = ["claude", "codex", "copilot", "muse"];
export const APPROVAL_TTL_MS = 110_000;
const MAX_STEPS = 30;
const MAX_DIFFS = 50;
const MAX_SESSIONS = 8;

export interface Step {
  id: number;
  kind: StepKind;
  tool: string;
  target: string;
  /** Edit steps: the Live Diff. */
  diff?: FileDiff;
  failed?: boolean;
}

export interface CodingSession {
  key: string;
  agent: AgentId;
  sessionId: string;
  cwd: string;
  project: string;
  state: SessionState;
  steps: Step[];
  diffs: FileDiff[];
  finalLine: string;
  updatedAt: number;
}

export interface QuestionOption {
  label: string;
  description?: string;
}

export interface Question {
  question: string;
  header?: string;
  multiSelect: boolean;
  options: QuestionOption[];
}

export interface Approval {
  requestId: string;
  sessionKey: string;
  agent: AgentId;
  project: string;
  tool: string;
  target: string;
  questions: Question[] | null;
  expiresAt: number;
}

export interface CodingState {
  sessions: CodingSession[];
  approval: Approval | null;
}

export type Effect = { type: "ack" | "decline"; requestId: string };

export const emptyCoding = (): CodingState => ({ sessions: [], approval: null });

type Raw = Record<string, unknown>;
const obj = (v: unknown): Raw | null => (v && typeof v === "object" && !Array.isArray(v) ? (v as Raw) : null);
const str = (v: unknown): string => (typeof v === "string" ? v : "");

export function agentOf(p: Raw): AgentId {
  const a = str(p.soul_agent);
  return a in AGENTS ? (a as AgentId) : "claude";
}

export function projectOf(cwd: string): string {
  return cwd.split(/[\\/]/).filter(Boolean).pop() ?? "";
}

const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

export function stepKind(tool: string): StepKind {
  const t = tool.toLowerCase();
  if (t === "read" || t === "view" || t === "read_file" || t === "notebookread") return "read";
  if (t === "write" || t === "create" || t === "write_file") return "write";
  if (t.includes("edit") || t === "replace") return "edit";
  if (["bash", "powershell", "shell", "run_command", "run_shell_command", "exec_command", "terminal"].includes(t)) return "run";
  if (["grep", "glob", "search", "ls", "list_dir", "find", "codebase_search"].includes(t)) return "search";
  if (t.startsWith("web") || t === "fetch") return "web";
  return "other";
}

/** What a tool call works on, short (`mod.rs`, `cargo test`). */
export function targetOf(tool: string, input: Raw | null, cwd = ""): string {
  if (!input) return "";
  const path = str(input.file_path) || str(input.path) || str(input.notebook_path);
  if (path) {
    const kind = stepKind(tool);
    if (kind === "read" || kind === "edit" || kind === "write") return baseName(path);
    return relativeTo(path, cwd);
  }
  const s = str(input.command) || str(input.pattern) || str(input.query) || str(input.url) || str(input.description) || str(input.prompt);
  return s.replace(/\s+/g, " ").trim().slice(0, 80);
}

/** `C:\p\soul\src\lib.rs` under `C:\p\soul` → `src/lib.rs`. */
export function relativeTo(path: string, cwd: string): string {
  const norm = (s: string) => s.replace(/\\/g, "/").replace(/\/+$/, "");
  const p = norm(path);
  const c = norm(cwd);
  return c && p.toLowerCase().startsWith(c.toLowerCase() + "/") ? p.slice(c.length + 1) : p;
}

/** Valid `AskUserQuestion` input (full, not truncated by the relay), else `null`. */
export function parseQuestions(input: Raw | null): Question[] | null {
  const qs = input?.questions;
  if (!Array.isArray(qs) || qs.length === 0 || qs.length > 4) return null;
  const out: Question[] = [];
  for (const q of qs) {
    const o = obj(q);
    const text = str(o?.question);
    const options = Array.isArray(o?.options)
      ? (o!.options as unknown[]).map((x) => ({ label: str(obj(x)?.label), description: str(obj(x)?.description) || undefined }))
      : [];
    if (!text || text.endsWith("…") || options.length === 0 || options.some((x) => !x.label || x.label.endsWith("…"))) return null;
    out.push({ question: text, header: str(o?.header) || undefined, multiSelect: o?.multiSelect === true, options });
  }
  return out;
}

let nextStep = 1;

function sessionKey(p: Raw, agent: AgentId): string {
  return str(p.session_id) || `${agent}:${str(p.cwd)}`;
}

function upsert(state: CodingState, p: Raw, now: number): [CodingSession[], CodingSession] {
  const agent = agentOf(p);
  const key = sessionKey(p, agent);
  const found = state.sessions.find((s) => s.key === key);
  const cwd = str(p.cwd) || found?.cwd || "";
  const s: CodingSession = found
    ? { ...found, cwd, project: projectOf(cwd) || found.project, updatedAt: now }
    : { key, agent, sessionId: str(p.session_id), cwd, project: projectOf(cwd), state: "idle", steps: [], diffs: [], finalLine: "", updatedAt: now };
  const rest = state.sessions.filter((x) => x.key !== key);
  return [rest, s];
}

const put = (rest: CodingSession[], s: CodingSession) => [s, ...rest].slice(0, MAX_SESSIONS);

/** One hook payload → new state + what to tell Rust. */
export function applyHook(state: CodingState, p: Raw, now: number): { state: CodingState; effects: Effect[] } {
  const event = str(p.hook_event_name);
  const tool = str(p.tool_name);
  const input = obj(p.tool_input);
  const requestId = str(p.request_id);
  const [rest, s] = upsert(state, p, now);
  let approval = state.approval;
  const effects: Effect[] = [];
  const clearMine = () => {
    if (approval?.sessionKey === s.key) approval = null;
  };

  switch (event) {
    case "SessionStart":
      s.state = "idle";
      break;
    case "UserPromptSubmit":
      s.state = "thinking";
      s.steps = [];
      s.finalLine = "";
      clearMine();
      break;
    case "PreToolUse":
      s.state = "working";
      if (tool && tool !== "AskUserQuestion")
        s.steps = [...s.steps, { id: nextStep++, kind: stepKind(tool), tool, target: targetOf(tool, input, s.cwd) }].slice(-MAX_STEPS);
      break;
    case "PostToolUse": {
      s.state = "working";
      const diff = buildFileDiff(tool, input);
      if (diff) {
        s.diffs = [...s.diffs, diff].slice(-MAX_DIFFS);
        const target = targetOf(tool, input, s.cwd);
        const last = [...s.steps].reverse().find((x) => x.tool === tool && x.target === target && x.diff === undefined);
        s.steps = last
          ? s.steps.map((x) => (x === last ? { ...x, diff } : x))
          : [...s.steps, { id: nextStep++, kind: stepKind(tool), tool, target, diff }].slice(-MAX_STEPS);
      }
      break;
    }
    case "PostToolUseFailure": {
      const last = s.steps[s.steps.length - 1];
      if (last) s.steps = s.steps.map((x) => (x === last ? { ...x, failed: true } : x));
      break;
    }
    case "PermissionRequest": {
      if (!requestId) break;
      if (!DECIDES.includes(s.agent) || approval) {
        // Never two cards: the second request goes straight back to its terminal.
        effects.push({ type: "decline", requestId });
        break;
      }
      const questions = tool === "AskUserQuestion" ? parseQuestions(input) : null;
      if (tool === "AskUserQuestion" && !questions) {
        effects.push({ type: "decline", requestId });
        break;
      }
      s.state = questions ? "question" : "approval";
      approval = {
        requestId,
        sessionKey: s.key,
        agent: s.agent,
        project: s.project,
        tool,
        target: targetOf(tool, input, s.cwd),
        questions,
        expiresAt: now + APPROVAL_TTL_MS,
      };
      effects.push({ type: "ack", requestId });
      break;
    }
    case "Notification":
      if (/rate.?limit|usage limit/i.test(str(p.message))) s.state = "ratelimit";
      break;
    case "Stop":
      s.state = "finished";
      s.finalLine = toOneLine(str(p.last_assistant_message) || str(p.message) || str(p.response) || str(p.prompt_response));
      clearMine();
      break;
    case "StopFailure":
      s.state = /rate.?limit|429/i.test(JSON.stringify(p.error ?? p.message ?? "")) ? "ratelimit" : "error";
      clearMine();
      break;
    case "SessionEnd":
    case "Interrupt":
      s.state = "idle";
      clearMine();
      break;
  }
  return { state: { sessions: put(rest, s), approval }, effects };
}

/** The card was answered (click, voice) or went back to the terminal. */
export function applyDecided(state: CodingState, requestId: string): CodingState {
  if (state.approval?.requestId !== requestId) return state;
  const key = state.approval.sessionKey;
  return {
    approval: null,
    sessions: state.sessions.map((s) => (s.key === key && (s.state === "approval" || s.state === "question") ? { ...s, state: "working" } : s)),
  };
}

/** The session that drives the collapsed pill: working / thinking, most recent first. */
export function liveSession(state: CodingState): CodingSession | null {
  return state.sessions.find((s) => s.state === "working" || s.state === "thinking") ?? null;
}

/** `+12 −4` for an edit step. */
export function diffStat(d: FileDiff | undefined): string {
  return d ? `+${d.added} −${d.removed}` : "";
}
