export type GpuStat = {
  id: string;
  index: number;
  name: string | null;
  vendor: string;
  utilizationPct: number | null;
  memoryUsedBytes: number | null;
  memoryTotalBytes: number | null;
  temperatureC: number | null;
  powerWatts: number | null;
};

/** Latest utilization for one rig in this rig's group (mirror lib.rs
 * GroupRigMetricsView / the list_group_rig_metrics RPC). `gpus` are the raw
 * rig_metrics jsonb objects — a superset of GpuStat, only some fields used. */
export type GroupRigMetrics = {
  rigId: string;
  name: string | null;
  /** True for the local rig itself (resolved server-side). */
  isSelf: boolean;
  lastSeen: string | null;
  cpuUtilizationPct: number | null;
  gpus: GpuStat[];
};

export type DiskStat = {
  id: string;
  name: string;
  displayName: string;
  mountPoints: string[];
  kind: "nvme" | "hdd" | "ssd" | "unknown";
  transport: string | null;
  removable: boolean;
  usedBytes: number | null;
  totalBytes: number;
  readBytesPerSecond: number | null;
  writeBytesPerSecond: number | null;
  totalReadBytes: number | null;
  totalWrittenBytes: number | null;
};

export type NetworkStat = {
  id: string;
  name: string;
  displayName: string;
  hardwareName: string | null;
  interfaceType: "ethernet" | "wifi" | "network";
  macAddress: string | null;
  ipAddresses: string[];
  receivedBytesPerSecond: number | null;
  transmittedBytesPerSecond: number | null;
  totalReceivedBytes: number;
  totalTransmittedBytes: number;
};

export type SystemMetricsSnapshot = {
  sampledAtMs: number;
  cpuUtilizationPct: number | null;
  memoryUsedBytes: number | null;
  memoryTotalBytes: number | null;
  gpus: GpuStat[];
  disks: DiskStat[];
  networks: NetworkStat[];
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

export type AgentUpdateInfo = {
  currentVersion: string;
  latestVersion: string;
  os: string;
  installCommand: string;
};

export type LlamaCppUpdateInfo = {
  currentTag: string | null;
  latestTag: string;
  backend: string;
  variant: string | null;
  sizeBytes: number;
  installable: boolean;
  reason: string | null;
};

export type LlamaCppUpdateProgress = {
  phase: "checking" | "downloading" | "verifying" | "installing" | "reloading" | "complete" | "error";
  tag: string;
  downloadedBytes: number;
  totalBytes: number;
  message: string | null;
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

/**
 * Reasoning intensity, matching llama-server's `reasoning_effort` values.
 * `"none"` disables reasoning; graded levels are only honored by reasoning-
 * trained models (mirror src-tauri/src/runtime/mod.rs `ReasoningEffort`).
 */
export type ReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max";

/** Ordered levels for the effort selector, low → high. */
export const REASONING_EFFORTS: ReasoningEffort[] = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

export type SessionSettings = {
  systemPrompt: string | null;
  temperature: number | null;
  numCtx: number | null;
  think: boolean;
  reasoningEffort: ReasoningEffort | null;
  webTools: boolean;
  mcp: boolean;
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
  contextNotes: string | null;
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
  contextState: ChatContextState;
};

export type ChatContextState = {
  checkpoint: string | null;
  summarizedThroughMessageCount: number;
  autoCompact: boolean;
  autoThreshold: number;
  lastCompactedAt: string | null;
};

/** Streamed delta events emitted by the backend on the "local-chat" event. */
export type LocalChatEvent = { requestId: string; sessionId: string } & (
  | { type: "content"; delta: string }
  | { type: "thinking"; delta: string }
  | { type: "tool"; name: string; arguments: string }
  | { type: "tool_result"; name: string; summary: string }
  | { type: "context_updated"; context: ChatContextInfo }
  | { type: "compaction_started"; reason: "manual" | "auto" }
  | { type: "compaction_completed"; reason: "manual" | "auto" }
  | { type: "compaction_failed"; message: string }
);

// --- Coding sessions (cloud-backed; mirror supabase 0036 + shared chat.ts) --

export type ApprovalPolicy = "read_only" | "plan" | "approve_writes" | "auto";

/** A local model offered in the Code/Chat model pickers. */
export type ModelOption = { name: string; loaded: boolean };

// Local (offline) coding sessions — camelCase, mirror src-tauri/src/coding_store.rs.
export type CodingSessionMeta = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  model: string;
  workspaceRoot: string;
  approvalPolicy: ApprovalPolicy;
  messageCount: number;
};

export type CodingStoredMessage = {
  role: "system" | "user" | "assistant";
  content: string;
  thinking: string | null;
  toolActivity: unknown;
  contextNotes: string | null;
  cancelled: boolean;
  createdAt: string;
};

export type CodingSession = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  model: string;
  workspaceRoot: string;
  approvalPolicy: ApprovalPolicy;
  mcpEnabled: boolean;
  reasoningEffort: ReasoningEffort | null;
  messages: CodingStoredMessage[];
  contextState: CodingContextState;
};

export type AgentScope = "builtin" | "project" | "global";

/** A sub-agent shown in the Code tab's Agents panel (mirror lib.rs AgentView). */
export type AgentView = {
  name: string;
  description: string;
  tools: string[];
  prompt: string;
  scope: AgentScope;
  editable: boolean;
  /** Per-agent exploration round budget (4–64), or null to use the default. */
  maxRounds: number | null;
};

/** A serving group peer this rig can offload sub-agent inference to
 * (mirror peers::PeerInfo). */
export type PeerInfo = {
  rigId: string;
  name: string | null;
  groupId: string;
  model: string | null;
  ready: boolean;
};

/** Distributed sub-agent status for the Code tab (mirror lib.rs
 * GroupSubagentStatus). */
export type GroupSubagentStatus = {
  /** Consumer-side opt-in: dispatch this rig's sub-agents to group peers. */
  enabled: boolean;
  /** Producer-side: this rig accepts relayed jobs (owner-set in the dashboard). */
  serving: boolean;
  peers: PeerInfo[];
};

// ---- MCP servers (mirrors src-tauri/src/mcp + the mcp_* Tauri commands) ----

export type McpTrust = "untrusted" | "trusted";
export type McpStatus = "stopped" | "starting" | "running" | "failed";

export type McpTransport =
  | { kind: "stdio"; command: string; args: string[]; env: Record<string, string>; cwd?: string | null }
  | { kind: "streamableHttp"; url: string };

export type McpToolView = {
  name: string;
  qualified: string;
  description: string;
  enabled: boolean;
  available: boolean;
  mutating: boolean;
  schemaTokens: number;
};

export type McpServerView = {
  id: string;
  label: string;
  enabled: boolean;
  trust: McpTrust;
  status: McpStatus;
  toolCount: number;
  lastError: string | null;
  transport: McpTransport;
  disabledTools: string[];
  catalogId: string | null;
  secretKeys: string[];
  tools: McpToolView[];
};

export type McpInputSpec = {
  key: string;
  label: string;
  required: boolean;
  secret: boolean;
  placeholder: string;
};

export type McpCatalogEntry = {
  id: string;
  label: string;
  description: string;
  details: string;
  connection: string;
  tools: { name: string; description: string }[];
  runtime: "npx" | "uvx";
  inputs: McpInputSpec[];
  defaultTrust: McpTrust;
  caveat: string | null;
  command: string;
};

export type McpRuntimeAvailability = { node: boolean; uv: boolean };

export type McpOverview = {
  servers: McpServerView[];
  truncated: number;
  toolLimit: number;
  availableTools: number;
  activeSchemaTokens: number;
  availableSchemaTokens: number;
  runtimes: McpRuntimeAvailability;
  catalog: McpCatalogEntry[];
};

/** A configured server as sent to mcp_add_server (BYO stdio). */
export type McpServerConfig = {
  id: string;
  label: string;
  transport: McpTransport;
  enabled: boolean;
  trust: McpTrust;
  disabledTools: string[];
  catalogId?: string | null;
};

export type CodingContextState = {
  checkpoint: string | null;
  summarizedThroughMessageId: string | null;
  latestUsedTokens: number | null;
  maxTokens: number | null;
  countExact: boolean;
  reserveTokens: number | null;
  tokenEstimateScale: number | null;
  autoCompact: boolean;
  autoThreshold: number;
  lastCompactedAt: string | null;
};

export type CodingContextInfo = {
  usedTokens: number;
  maxTokens: number;
  reserveTokens: number;
  percent: number;
  level: "normal" | "orange" | "red";
  countExact: boolean;
  autoCompact: boolean;
  autoThreshold: number;
  compacted: boolean;
  status: "idle" | "compacting";
  mcpTools: number;
  mcpSchemaTokens: number;
};

export type ChatContextInfo = {
  usedTokens: number;
  maxTokens: number;
  reserveTokens: number;
  percent: number;
  level: "normal" | "orange" | "red";
  countExact: boolean;
  autoCompact: boolean;
  autoThreshold: number;
  compacted: boolean;
  status: "idle" | "compacting";
  mcpTools: number;
  mcpSchemaTokens: number;
};

export type CodingPreviewStatus = {
  sessionId: string;
  windowOpen: boolean;
  url: string | null;
  serverState: "stopped" | "starting" | "ready";
  serverCommand: string | null;
};

/**
 * The `event` payload inside a Tauri "local-coding" event. Mirrors the coding
 * subset of the shared ChatStreamEvent union (packages/shared/src/chat.ts).
 */
export type CodingStreamEvent =
  | { type: "loading"; model: string }
  | { type: "token"; delta: string }
  | { type: "thinking"; delta: string }
  | { type: "tool"; name: string; arguments: string }
  | { type: "tool_result"; name: string; summary: string }
  | { type: "file_edit"; path: string; diff: string; summary: string; invocationId?: string }
  | { type: "command"; command: string; chunk: string; exitCode?: number | null; invocationId?: string }
  | { type: "subagent_started"; agent: string; task: string }
  | { type: "subagent_result"; agent: string; summary: string }
  | { type: "approval_needed"; invocationId: string; name: string; preview: string }
  | { type: "approval_resolved"; invocationId: string; decision: "approved" | "denied" }
  | { type: "context_updated"; context: CodingContextInfo }
  | { type: "compaction_started"; reason: "manual" | "auto" }
  | { type: "compaction_completed"; reason: "manual" | "auto" }
  | { type: "compaction_failed"; message: string }
  | { type: "done" }
  | { type: "error"; message: string };

/** Envelope the agent emits on the Tauri "local-coding" event. */
export type CodingEvent = {
  sessionId: string;
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
    contextNotes: null,
    cancelled: false,
    createdAt: new Date().toISOString(),
  };
}

export function formatGB(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}
