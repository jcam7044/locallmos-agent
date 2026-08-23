import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  AgentView,
  GroupRigMetrics,
  GroupSubagentStatus,
  ApprovalPolicy,
  Attachment,
  ChatSession,
  CodingSession,
  CodingContextInfo,
  CodingSessionMeta,
  CodingStoredMessage,
  CodingPreviewStatus,
  LocalStatus,
  SessionMeta,
  SessionSettings,
  StoredMessage,
  DownloadState,
  HubModelDetail,
  HubModelPage,
  ModelLoadSettings,
  GpuDeviceList,
  McpOverview,
  McpServerConfig,
  McpTrust,
  ReasoningEffort,
  ChatContextInfo,
  AgentUpdateInfo,
  LlamaCppUpdateInfo,
  SystemMetricsSnapshot,
} from "./types";

export const getLocalStatus = () => invoke<LocalStatus>("local_status");
export const getAgentStatus = () => invoke<AgentStatus>("get_status");
export const getSystemMetricsSnapshot = () =>
  invoke<SystemMetricsSnapshot>("system_metrics_snapshot");
export const openResourcesWindow = () => invoke<void>("open_resources_window");
export const loadModel = (model: string) => invoke("load_model", { model });
export const unloadModel = (model: string) => invoke("unload_model", { model });
export const getModelLoadSettings = (modelId: string) =>
  invoke<ModelLoadSettings>("get_model_load_settings", { modelId });
export const saveModelLoadSettings = (
  modelId: string,
  settings: ModelLoadSettings,
  loadNow: boolean,
) => invoke("save_model_load_settings", { modelId, settings, loadNow });
export const listGpuDevices = () => invoke<GpuDeviceList>("list_gpu_devices");
export const setGpuDefault = (selection: string[] | null) =>
  invoke("set_gpu_default", { selection });
export const deleteLocalModel = (modelId: string) => invoke("delete_local_model", { modelId });
export const ollamaPullModel = (model: string) => invoke("ollama_pull_model", { model });
export const restartRuntime = () => invoke("restart_runtime");
export const setRuntime = (kind: string) => invoke("set_runtime", { kind });
export const openModelsDir = () => invoke("open_models_dir");
export const hubSearchModels = (args: {
  query: string;
  capability: string;
  sort: string;
  cursor?: string | null;
}) => invoke<HubModelPage>("hub_search_models", args);
export const hubGetModel = (repoId: string) =>
  invoke<HubModelDetail>("hub_get_model", { repoId });
export const hubGetAuthorAvatars = (authors: string[]) =>
  invoke<Record<string, string>>("hub_get_author_avatars", { authors });
export const hubStartDownload = (repoId: string, revision: string, variantId: string) =>
  invoke<DownloadState>("hub_start_download", { repoId, revision, variantId });
export const hubListDownloads = () => invoke<DownloadState[]>("hub_list_downloads");
export const hubCancelDownload = (id: string) => invoke<DownloadState>("hub_cancel_download", { id });
export const agentCheckUpdate = () => invoke<AgentUpdateInfo | null>("agent_check_update");
export const llamaCppCheckUpdate = () =>
  invoke<LlamaCppUpdateInfo | null>("llamacpp_check_update");
export const llamaCppInstallUpdate = (tag: string) =>
  invoke<void>("llamacpp_install_update", { tag });
export const enroll = (code: string, name: string) => invoke("enroll", { code, name });
export const localChatSend = (args: {
  sessionId: string;
  requestId: string;
  content: string;
  attachments?: Attachment[];
  regenerate?: boolean;
}) => invoke<StoredMessage>("local_chat_send", args);

export const localChatCancel = (requestId: string) =>
  invoke("local_chat_cancel", { requestId });

export const chatGetContext = (sessionId: string) =>
  invoke<ChatContextInfo>("chat_context", { sessionId });

export const chatCompact = (sessionId: string) =>
  invoke<ChatContextInfo>("chat_compact", { sessionId });

export const chatSetContextSettings = (
  sessionId: string,
  autoCompact: boolean,
  autoThreshold: number,
) => invoke<ChatContextInfo>("chat_set_context_settings", {
  sessionId,
  autoCompact,
  autoThreshold,
});

export const readDroppedFile = (path: string) =>
  invoke<Attachment>("read_dropped_file", { path });

export const chatListSessions = () => invoke<SessionMeta[]>("chat_list_sessions");
export const chatCreateSession = (model: string) =>
  invoke<ChatSession>("chat_create_session", { model });
export const chatGetSession = (id: string) => invoke<ChatSession>("chat_get_session", { id });
export const chatRenameSession = (id: string, title: string) =>
  invoke("chat_rename_session", { id, title });
export const chatDeleteSession = (id: string) => invoke("chat_delete_session", { id });
export const chatUpdateSettings = (id: string, model: string, settings: SessionSettings) =>
  invoke("chat_update_settings", { id, model, settings });

// --- Local coding sessions (offline, on-disk; no cloud required) ------------

export const codingCreateSession = (
  workspacePath: string,
  model: string,
  approvalPolicy: ApprovalPolicy,
) => invoke<CodingSession>("coding_local_create_session", { workspacePath, model, approvalPolicy });

export const codingSetPolicy = (id: string, policy: ApprovalPolicy) =>
  invoke<CodingSession>("coding_local_set_policy", { id, policy });

/** Validate picked paths against the session workspace; returns them relative. */
export const codingAttach = (id: string, paths: string[]) =>
  invoke<string[]>("coding_local_attach", { id, paths });

export const codingListSessions = () =>
  invoke<CodingSessionMeta[]>("coding_local_list_sessions");

export const codingGetSession = (id: string) =>
  invoke<CodingSession>("coding_local_get_session", { id });

export const codingGetContext = (sessionId: string) =>
  invoke<CodingContextInfo>("coding_local_context", { sessionId });

export const codingCompact = (sessionId: string) =>
  invoke<CodingContextInfo>("coding_local_compact", { sessionId });

export const codingSetContextSettings = (
  sessionId: string,
  autoCompact: boolean,
  autoThreshold: number,
) => invoke<CodingContextInfo>("coding_local_set_context_settings", {
  sessionId,
  autoCompact,
  autoThreshold,
});

export const codingDeleteSession = (id: string) =>
  invoke("coding_local_delete_session", { id });

export const codingSend = (sessionId: string, requestId: string, content: string) =>
  invoke<CodingStoredMessage>("coding_local_send", { sessionId, requestId, content });

export const codingApprove = (invocationId: string, approved: boolean) =>
  invoke("coding_local_approve", { invocationId, approved });

export const codingCancel = (requestId: string) =>
  invoke("coding_local_cancel", { requestId });

export const codingListAgents = (workspaceRoot: string) =>
  invoke<AgentView[]>("coding_list_agents", { workspaceRoot });

export const codingSaveAgent = (
  workspaceRoot: string,
  scope: "project" | "global",
  name: string,
  description: string,
  prompt: string,
  tools: string[],
  maxRounds: number | null,
) => invoke("coding_save_agent", { workspaceRoot, scope, name, description, prompt, tools, maxRounds });

export const codingDeleteAgent = (
  workspaceRoot: string,
  scope: "project" | "global",
  name: string,
) => invoke("coding_delete_agent", { workspaceRoot, scope, name });

export const codingPeerStatus = () =>
  invoke<GroupSubagentStatus>("coding_peer_status");

export const codingGroupRigMetrics = () =>
  invoke<GroupRigMetrics[]>("coding_group_rig_metrics");

export const codingSetUseGroupSubagents = (enabled: boolean) =>
  invoke("coding_set_use_group_subagents", { enabled });

export const codingPreviewStatus = (sessionId: string) =>
  invoke<CodingPreviewStatus>("coding_preview_status", { sessionId });

export const codingPreviewFocus = (sessionId: string) =>
  invoke("coding_preview_focus", { sessionId });

export const codingPreviewReload = (sessionId: string) =>
  invoke("coding_preview_reload", { sessionId });

export const codingPreviewClose = (sessionId: string) =>
  invoke("coding_preview_close", { sessionId });

export const codingSetMcpEnabled = (id: string, enabled: boolean) =>
  invoke<CodingSession>("coding_local_set_mcp_enabled", { id, enabled });

export const codingSetReasoningEffort = (id: string, effort: ReasoningEffort | null) =>
  invoke<CodingSession>("coding_local_set_reasoning_effort", { id, effort });

// ---- MCP server management ----

export const mcpOverview = () => invoke<McpOverview>("mcp_overview");

export const mcpSetToolLimit = (limit: number) =>
  invoke<McpOverview>("mcp_set_tool_limit", { limit });

export const mcpAddServer = (config: McpServerConfig) =>
  invoke<McpOverview>("mcp_add_server", { config });

export const mcpInstallCatalogEntry = (
  catalogId: string,
  serverId: string,
  inputs: Record<string, string>,
) => invoke<McpOverview>("mcp_install_catalog_entry", { catalogId, serverId, inputs });

export const mcpUpdateServer = (args: {
  id: string;
  enabled?: boolean;
  trust?: McpTrust;
  disabledTools?: string[];
  label?: string;
}) => invoke<McpOverview>("mcp_update_server", args);

export const mcpSetSecret = (id: string, key: string, value: string) =>
  invoke("mcp_set_secret", { id, key, value });

export const mcpDeleteServer = (id: string) =>
  invoke<McpOverview>("mcp_delete_server", { id });

export const mcpStartServer = (id: string) =>
  invoke<McpOverview>("mcp_start_server", { id });

export const mcpStopServer = (id: string) =>
  invoke<McpOverview>("mcp_stop_server", { id });

export const mcpServerLogs = (id: string) =>
  invoke<string>("mcp_server_logs", { id });
