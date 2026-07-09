import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  Attachment,
  ChatSession,
  LocalStatus,
  SessionMeta,
  SessionSettings,
  StoredMessage,
} from "./types";

export const getLocalStatus = () => invoke<LocalStatus>("local_status");
export const getAgentStatus = () => invoke<AgentStatus>("get_status");
export const loadModel = (model: string) => invoke("load_model", { model });
export const restartRuntime = () => invoke("restart_runtime");
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
