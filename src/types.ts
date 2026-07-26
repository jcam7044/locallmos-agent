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
  id: string;
  name: string;
  sizeBytes: number | null;
  quantization: string | null;
  loaded: boolean;
  capabilities: string[];
  sourceRepo: string | null;
  revision: string | null;
  variantId: string | null;
  files: string[];
};

export type LocalStatus = {
  runtime: {
    kind: string;
    version: string | null;
    /** Active llama.cpp acceleration backend (cuda|rocm|vulkan|cpu|metal); null for Ollama. */
    backend: string | null;
    state: string;
    endpoint: string | null;
    modelsDir: string | null;
    contextSize: number | null;
  };
  modelsStorage: {
    dir: string;
    availableBytes: number | null;
    totalBytes: number | null;
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

export type HubModelSummary = {
  id: string;
  author: string;
  name: string;
  revision: string;
  downloads: number;
  likes: number;
  lastModified: string | null;
  pipelineTag: string | null;
  tags: string[];
  avatarUrl: string;
};

export type GgufFile = { path: string; sizeBytes: number };
export type MemoryEstimate = {
  weightsBytes: number;
  kvCacheBytes: number;
  overheadBytes: number;
  totalBytes: number;
  confidence: "high" | "low";
};
export type GgufVariant = {
  id: string;
  quantization: string;
  sizeBytes: number;
  files: GgufFile[];
  companions: GgufFile[];
  memory: MemoryEstimate;
};
export type HubModelDetail = HubModelSummary & {
  license: string | null;
  baseModels: string[];
  readmeMarkdown: string;
  variants: GgufVariant[];
};
export type HubModelPage = { items: HubModelSummary[]; nextCursor: string | null };
export type DownloadState = {
  id: string;
  repoId: string;
  revision: string;
  variantId: string;
  status: "queued" | "downloading" | "canceling" | "cancelled" | "complete" | "error";
  downloadedBytes: number;
  totalBytes: number;
  error: string | null;
};

export type OllamaPullProgress = {
  model: string;
  status: string;
  completed: number | null;
  total: number | null;
};

export type ModelLoadSettings = {
  contextSize: number | null;
  kvCacheType: "auto" | "f16" | "q8_0" | "q4_0";
  gpuOffload: "auto" | "all" | "cpu_only";
  flashAttention: "auto" | "on" | "off";
  cpuThreads: number | null;
  speculativeDecoding: "auto" | "off" | "mtp";
  /** Empty uses the recommended 25-call limit. */
  maxToolCalls: number | null;
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

/** Performance details reported by llama.cpp for a completed response. */
export type GenerationMetrics = {
  promptEvalTokens: number | null;
  cachedTokens: number | null;
  promptEvalMs: number | null;
  promptTokensPerSecond: number | null;
  generationMs: number | null;
  tokensPerSecond: number | null;
  timeToFirstTokenMs: number | null;
  streamChunks: number;
};

export type StoredMessage = {
  role: "user" | "assistant";
  content: string;
  thinking: string | null;
  attachments: Attachment[];
  promptTokens: number | null;
  completionTokens: number | null;
  generationMetrics: GenerationMetrics | null;
  toolLimitReached: number | null;
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

// --- Coding sessions (cloud-backed; mirror supabase 0036 + shared chat.ts) --

export type ApprovalPolicy = "plan" | "approve_writes" | "auto";

export type CodingWorkspace = {
  id: string;
  name: string;
  root_path: string;
  approval_policy: ApprovalPolicy;
};

export type CodingSessionMeta = {
  id: string;
  title: string | null;
  model: string | null;
  workspace_id: string | null;
  updated_at: string;
};

export type CodingMessage = {
  id: string;
  role: "system" | "user" | "assistant";
  content: string;
  status: "pending" | "streaming" | "done" | "error";
  thinking: string | null;
  tool_activity: unknown;
  created_at: string;
};

/**
 * The `event` payload inside a Tauri "coding" event. Mirrors the coding subset
 * of the shared ChatStreamEvent union (packages/shared/src/chat.ts).
 */
export type CodingStreamEvent =
  | { type: "loading"; model: string }
  | { type: "token"; delta: string }
  | { type: "thinking"; delta: string }
  | { type: "tool"; name: string; arguments: string }
  | { type: "tool_result"; name: string; summary: string }
  | { type: "file_edit"; path: string; diff: string; summary: string; invocationId?: string }
  | { type: "command"; command: string; chunk: string; exitCode?: number | null; invocationId?: string }
  | { type: "approval_needed"; invocationId: string; name: string; preview: string }
  | { type: "approval_resolved"; invocationId: string; decision: "approved" | "denied" }
  | { type: "done" }
  | { type: "error"; message: string };

/** Envelope the agent emits on the Tauri "coding" event. */
export type CodingEvent = {
  conversationId: string;
  messageId: string;
  event: CodingStreamEvent;
};

export function newUserMessage(content: string, attachments: Attachment[] = []): StoredMessage {
  return {
    role: "user",
    content,
    thinking: null,
    attachments,
    promptTokens: null,
    completionTokens: null,
    generationMetrics: null,
    toolLimitReached: null,
    toolActivity: null,
    cancelled: false,
    createdAt: new Date().toISOString(),
  };
}

export function formatGB(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}
