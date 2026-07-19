export type GpuStat = {
  index: number;
  name: string | null;
  vendor: string;
  utilizationPct: number | null;
  memoryUsedBytes: number | null;
  memoryTotalBytes: number | null;
  temperatureC: number | null;
  powerWatts: number | null;
};

export type AgentStatus = {
  enrolled: boolean;
  rigName: string | null;
  connected: boolean;
};

export type LocalModel = {
  name: string;
  sizeBytes: number | null;
  quantization: string | null;
  loaded: boolean;
  capabilities: string[];
};

export type LocalStatus = {
  runtime: {
    kind: string;
    version: string | null;
    state: string;
    endpoint: string | null;
    modelsDir: string | null;
  };
  /** Persisted runtime choice; may differ from the active `runtime.kind` until restart. */
  configuredRuntime: string | null;
  models: LocalModel[];
  telemetry: {
    cpuPct: number | null;
    memoryUsedBytes: number | null;
    memoryTotalBytes: number | null;
    gpus: GpuStat[];
  };
};

// --- Persistent chat sessions (mirror src-tauri/src/chat_store.rs) ---------

export type SessionSettings = {
  systemPrompt: string | null;
  temperature: number | null;
  numCtx: number | null;
  think: boolean;
  webTools: boolean;
};

export type Attachment = {
  kind: "image" | "text";
  name: string;
  mime: string;
  sizeBytes: number;
  data: string | null;
  text: string | null;
};

export type StoredMessage = {
  role: "user" | "assistant";
  content: string;
  thinking: string | null;
  attachments: Attachment[];
  promptTokens: number | null;
  completionTokens: number | null;
  toolActivity: unknown;
  cancelled: boolean;
  createdAt: string;
};

export type SessionMeta = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  model: string;
  messageCount: number;
};

export type ChatSession = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  model: string;
  settings: SessionSettings;
  messages: StoredMessage[];
};

/** Streamed delta events emitted by the backend on the "local-chat" event. */
export type LocalChatEvent = { requestId: string; sessionId: string } & (
  | { type: "content"; delta: string }
  | { type: "thinking"; delta: string }
  | { type: "tool"; name: string; arguments: string }
  | { type: "tool_result"; name: string; summary: string }
);

export function newUserMessage(content: string, attachments: Attachment[] = []): StoredMessage {
  return {
    role: "user",
    content,
    thinking: null,
    attachments,
    promptTokens: null,
    completionTokens: null,
    toolActivity: null,
    cancelled: false,
    createdAt: new Date().toISOString(),
  };
}

export function formatGB(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}
