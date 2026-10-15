/** Settings data model + Gemini model helpers (mirrors config.rs / gemini.rs rules). */

export interface IslandSections {
  nowPlaying: boolean;
  clipboard: boolean;
  plans: boolean;
  quickApps: boolean;
  quickSettings: boolean;
}

export type Personality = "neutral" | "ziomek" | "professional" | "sarcastic";

export interface SettingsData {
  apiKey: string;
  hotkey: string;
  askHotkey: string;
  agentHotkey: string;
  model: string;
  fallbackModel: string;
  dictationModel?: string;
  commandsModel?: string;
  askModel?: string;
  useLiveApi?: boolean;
  language: string;
  microphone: string;
  retiredModels?: string[];
  modelNotice?: string | null;
  islandEnabled?: boolean;
  islandPosition?: "topCenter" | "topLeft" | "topRight";
  islandSections?: IslandSections;
  pinnedApps?: string[];
  talkMode?: "smart" | "separate";
  talkKey?: string;
  talkKeySwallow?: boolean;
  talkDragAsk?: boolean;
  wakeWord?: string;
  voiceEnabled?: boolean;
  voiceName?: string;
  voiceSpeed?: "slow" | "normal" | "fast";
  voiceBrief?: boolean;
  personality?: Personality;
  /** Soul's own voice volume, 0–100. */
  voiceVolume?: number;
  duckMusic?: boolean;
  searchOpenBrowser?: boolean;
  autoRemember?: boolean;
  /** Follow-ups: remember the last 3 command exchanges. */
  contextEnabled?: boolean;
  /** Forget the conversation context after this many seconds without a command (15–600). */
  contextTimeoutSecs?: number;
}

export interface MemoryFact {
  id: number;
  text: string;
  category: string;
  source: string;
  createdAt: number;
  updatedAt: number;
  lastUsedAt: number;
  useCount: number;
}

export const MEMORY_CATEGORIES: { id: string; pl: string; en: string }[] = [
  { id: "gry", pl: "Gry", en: "Games" },
  { id: "muzyka", pl: "Muzyka", en: "Music" },
  { id: "praca", pl: "Praca", en: "Work" },
  { id: "osobiste", pl: "Osobiste", en: "Personal" },
  { id: "preferencje", pl: "Preferencje", en: "Preferences" },
  { id: "notatka", pl: "Notatki", en: "Notes" },
  { id: "inne", pl: "Inne", en: "Other" },
];

export type SceneStepType = "open_app" | "open_url" | "set_volume" | "media" | "youtube_play" | "web_search" | "delay" | "dnd";

export interface SceneStep {
  type: SceneStepType;
  value: string;
}

export interface Scene {
  id: number;
  name: string;
  triggers: string[];
  steps: SceneStep[];
  updatedAt?: number;
}

export const STEP_TYPES: { id: SceneStepType; pl: string; en: string; placeholder: string }[] = [
  { id: "open_app", pl: "Otwórz aplikację", en: "Open app", placeholder: "Discord" },
  { id: "open_url", pl: "Otwórz link", en: "Open link", placeholder: "https://open.spotify.com/playlist/…" },
  { id: "youtube_play", pl: "YouTube ▶", en: "YouTube ▶", placeholder: "lofi hip hop" },
  { id: "web_search", pl: "Wyszukaj", en: "Search", placeholder: "pogoda Kraków" },
  { id: "set_volume", pl: "Głośność systemu %", en: "System volume %", placeholder: "40" },
  { id: "media", pl: "Muzyka", en: "Media", placeholder: "play" },
  { id: "delay", pl: "Odczekaj (ms)", en: "Wait (ms)", placeholder: "2000" },
  { id: "dnd", pl: "Nie przeszkadzać", en: "Do not disturb", placeholder: "on" },
];

export interface GeminiModelInfo {
  id: string;
  displayName: string;
  supportsLive?: boolean;
  supportsGenerate?: boolean;
}

export interface MicrophoneInfo {
  name: string;
  isDefault: boolean;
}

/** Live API prebuilt voices (mirrors VOICE_NAMES in config.rs). */
export const VOICES: { id: string; pl: string; en: string }[] = [
  { id: "Charon", pl: "głęboki, spokojny", en: "deep, calm" },
  { id: "Puck", pl: "pogodny", en: "upbeat" },
  { id: "Kore", pl: "stanowczy, wyraźny", en: "firm, clear" },
  { id: "Fenrir", pl: "energiczny", en: "energetic" },
  { id: "Aoede", pl: "lekki, zwiewny", en: "light, breezy" },
  { id: "Leda", pl: "młodzieńczy", en: "youthful" },
  { id: "Orus", pl: "zrównoważony", en: "steady" },
  { id: "Zephyr", pl: "jasny", en: "bright" },
];

export const PERSONALITIES: { id: Personality; pl: string; en: string; sample: string }[] = [
  { id: "neutral", pl: "Neutralny", en: "Neutral", sample: "„Otwieram Spotify.”" },
  { id: "ziomek", pl: "Ziomek", en: "Buddy", sample: "„Spoko, już odpalam Spotify!”" },
  { id: "professional", pl: "Profesjonalny", en: "Professional", sample: "„Uruchamiam Spotify.”" },
  { id: "sarcastic", pl: "Sarkastyczny", en: "Sarcastic", sample: "„Spotify, bo cisza była zbyt piękna.”" },
];

export const TALK_DEFAULTS = {
  talkMode: "smart" as const,
  talkKey: "ControlRight",
  talkKeySwallow: false,
  talkDragAsk: true,
  wakeWord: "Soul",
  voiceEnabled: true,
  voiceName: "Charon",
  voiceSpeed: "normal" as const,
  voiceBrief: false,
  personality: "neutral" as Personality,
  voiceVolume: 100,
  duckMusic: false,
  searchOpenBrowser: true,
  autoRemember: true,
  contextEnabled: true,
  contextTimeoutSecs: 90,
};

export const DEFAULT_PRIMARY = "gemini-3.1-flash-lite";
export const DEFAULT_ASK = "gemini-3.1-flash-lite";
export const DEFAULT_ASK_FALLBACK = "gemini-3.5-flash-lite";
export const DEFAULT_DICTATION = "gemini-3.5-transcribe-live";
export const DEFAULT_COMMANDS = "gemini-3.8-live";

const DEFAULT_SECTIONS: IslandSections = {
  nowPlaying: true,
  clipboard: true,
  plans: true,
  quickApps: true,
  quickSettings: true,
};

export const INITIAL_FORM: SettingsData = {
  apiKey: "",
  hotkey: "Ctrl+Super+Space",
  askHotkey: "Ctrl+Shift+Space",
  agentHotkey: "Super+Alt+Space",
  model: DEFAULT_ASK,
  askModel: DEFAULT_ASK,
  dictationModel: DEFAULT_DICTATION,
  commandsModel: DEFAULT_COMMANDS,
  fallbackModel: DEFAULT_ASK_FALLBACK,
  language: "auto",
  microphone: "",
  retiredModels: [],
  modelNotice: null,
  islandEnabled: true,
  islandPosition: "topCenter",
  islandSections: DEFAULT_SECTIONS,
  pinnedApps: [],
  ...TALK_DEFAULTS,
};

/** Fill gaps in a loaded settings object (older stores). */
export function withDefaults(s: SettingsData): SettingsData {
  return {
    ...s,
    hotkey: s.hotkey || "Ctrl+Super+Space",
    askHotkey: s.askHotkey || "Ctrl+Shift+Space",
    agentHotkey: s.agentHotkey || "Super+Alt+Space",
    fallbackModel: s.fallbackModel || DEFAULT_ASK_FALLBACK,
    askModel: s.askModel || s.model || DEFAULT_ASK,
    dictationModel: s.dictationModel || DEFAULT_DICTATION,
    commandsModel: s.commandsModel || DEFAULT_COMMANDS,
    microphone: s.microphone ?? "",
    retiredModels: s.retiredModels ?? [],
    modelNotice: s.modelNotice ?? null,
    islandEnabled: s.islandEnabled ?? true,
    islandPosition: s.islandPosition ?? "topCenter",
    islandSections: s.islandSections ?? DEFAULT_SECTIONS,
    pinnedApps: s.pinnedApps ?? [],
    talkMode: s.talkMode ?? TALK_DEFAULTS.talkMode,
    talkKey: s.talkKey || TALK_DEFAULTS.talkKey,
    talkKeySwallow: s.talkKeySwallow ?? TALK_DEFAULTS.talkKeySwallow,
    talkDragAsk: s.talkDragAsk ?? TALK_DEFAULTS.talkDragAsk,
    wakeWord: s.wakeWord || TALK_DEFAULTS.wakeWord,
    voiceEnabled: s.voiceEnabled ?? TALK_DEFAULTS.voiceEnabled,
    voiceName: s.voiceName || TALK_DEFAULTS.voiceName,
    voiceSpeed: s.voiceSpeed ?? TALK_DEFAULTS.voiceSpeed,
    voiceBrief: s.voiceBrief ?? TALK_DEFAULTS.voiceBrief,
    personality: s.personality ?? TALK_DEFAULTS.personality,
    voiceVolume: s.voiceVolume ?? TALK_DEFAULTS.voiceVolume,
    duckMusic: s.duckMusic ?? TALK_DEFAULTS.duckMusic,
    searchOpenBrowser: s.searchOpenBrowser ?? TALK_DEFAULTS.searchOpenBrowser,
    autoRemember: s.autoRemember ?? TALK_DEFAULTS.autoRemember,
    contextEnabled: s.contextEnabled ?? TALK_DEFAULTS.contextEnabled,
    contextTimeoutSecs: s.contextTimeoutSecs ?? TALK_DEFAULTS.contextTimeoutSecs,
  };
}

/** Offline stub list — never include retired generation ids (e.g. gemini-2.5-*). */
export const FALLBACK_MODELS: GeminiModelInfo[] = [
  { id: "gemini-3.5-transcribe-live", displayName: "Gemini 3.5 Transcribe Live", supportsLive: true },
  { id: "gemini-3.8-live", displayName: "Gemini 3.8 Live", supportsLive: true },
  { id: "gemini-3.1-flash-lite", displayName: "Gemini 3.1 Flash-Lite (Ask / REST)", supportsGenerate: true },
  { id: "gemini-3.5-flash-lite", displayName: "Gemini 3.5 Flash-Lite", supportsGenerate: true },
  { id: "gemini-flash-lite-latest", displayName: "Gemini Flash-Lite Latest", supportsGenerate: true },
  { id: "gemini-flash-latest", displayName: "Gemini Flash Latest", supportsGenerate: true },
];

/** Ctrl+Alt(+Shift)+letter collides with Polish AltGr (Right Alt = Ctrl+Alt on Windows). Mirrors config.rs. */
export function isCtrlAltLetterHotkey(hotkey: string): boolean {
  const parts = hotkey
    .replace(/\s+/g, "")
    .toLowerCase()
    .replace(/control/g, "ctrl")
    .replace(/command|cmd|win|meta/g, "super")
    .split("+")
    .filter(Boolean);
  const has = (...n: string[]) => parts.some((p) => n.includes(p));
  const hasKey = parts.some((p) => /^[a-z0-9]$/.test(p));
  return has("ctrl") && has("alt", "option") && !has("super") && hasKey;
}

/** Live-capable (bidiGenerateContent). Falls back to the id when the flag is missing. */
export function isLiveModel(m: GeminiModelInfo): boolean {
  if (m.supportsLive !== undefined) return m.supportsLive;
  return m.id.toLowerCase().includes("live");
}

export function isTranscribeLive(m: GeminiModelInfo): boolean {
  return isLiveModel(m) && m.id.toLowerCase().includes("transcribe");
}

export function isCommandsLive(m: GeminiModelInfo): boolean {
  const l = m.id.toLowerCase();
  return isLiveModel(m) && !l.includes("transcribe") && !l.includes("translate") && !l.includes("robotics");
}

/** REST models usable for Ask (vision + text). */
export function isAskModel(m: GeminiModelInfo): boolean {
  const l = m.id.toLowerCase();
  const generate = m.supportsGenerate ?? !isLiveModel(m);
  return generate && !l.includes("transcribe") && !l.includes("tts") && !l.includes("image");
}

function parseGeminiVersion(id: string): [number, number] | null {
  const m = id.match(/(\d+)\.(\d+)/);
  return m ? [Number(m[1]), Number(m[2])] : null;
}

function flashLiteRecencyKey(id: string): [number, number, number] {
  const ver = parseGeminiVersion(id);
  if (ver) return [2, ver[0], ver[1]];
  if (id.includes("latest")) return [1, 0, 0];
  return [0, 0, 0];
}

function isFlashLite(id: string): boolean {
  const lower = id.toLowerCase();
  return lower.includes("flash") && lower.includes("lite");
}

/** Newest Flash-Lite from the live list (prefers highest versioned id). */
export function pickNewestFlashLite(primary: string, models: GeminiModelInfo[], retired: string[] = []): string {
  const retiredSet = new Set(retired);
  const lites = models
    .filter((m) => isFlashLite(m.id) && m.id !== primary && !retiredSet.has(m.id))
    .sort((a, b) => {
      const ka = flashLiteRecencyKey(a.id);
      const kb = flashLiteRecencyKey(b.id);
      for (let i = 0; i < 3; i++) if (kb[i] !== ka[i]) return kb[i] - ka[i];
      return a.id.localeCompare(b.id);
    });
  if (lites[0]) return lites[0].id;
  const flash = models.find((m) => m.id !== primary && m.id.includes("flash"));
  if (flash) return flash.id;
  return models.find((m) => m.id !== primary)?.id ?? "";
}

export function pickPrimary(models: GeminiModelInfo[], retired: string[] = []): string {
  const retiredSet = new Set(retired);
  if (models.some((m) => m.id === DEFAULT_PRIMARY && !retiredSet.has(m.id))) return DEFAULT_PRIMARY;
  return pickNewestFlashLite("", models, retired) || models[0]?.id || DEFAULT_PRIMARY;
}
