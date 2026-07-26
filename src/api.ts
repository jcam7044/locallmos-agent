import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  ApprovalPolicy,
  Attachment,
  ChatSession,
  CodingSession,
  CodingSessionMeta,
  CodingStoredMessage,
  LocalStatus,
  SessionMeta,
  SessionSettings,
  StoredMessage,
  DownloadState,
  HubModelDetail,
  HubModelPage,
  ModelLoadSettings,
} from "./types";

export const getLocalStatus = () => invoke<LocalStatus>("local_status");
export const getAgentStatus = () => invoke<AgentStatus>("get_status");
export const loadModel = (model: string) => invoke("load_model", { model });
export const unloadModel = (model: string) => invoke("unload_model", { model });
export const getModelLoadSettings = (modelId: string) =>
  invoke<ModelLoadSettings>("get_model_load_settings", { modelId });
export const saveModelLoadSettings = (
  modelId: string,
  settings: ModelLoadSettings,
  loadNow: boolean,
) => invoke("save_model_load_settings", { modelId, settings, loadNow });
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
export const localUpdate = () => invoke<string | null>("local_update");
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

export const codingDeleteSession = (id: string) =>
  invoke("coding_local_delete_session", { id });

export const codingSend = (sessionId: string, requestId: string, content: string) =>
  invoke<CodingStoredMessage>("coding_local_send", { sessionId, requestId, content });

export const codingApprove = (invocationId: string, approved: boolean) =>
  invoke("coding_local_approve", { invocationId, approved });

export const codingCancel = (requestId: string) =>
  invoke("coding_local_cancel", { requestId });
