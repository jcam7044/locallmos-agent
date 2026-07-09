import { invoke } from "@tauri-apps/api/core";
import type { AgentStatus, ChatMsg, LocalStatus } from "./types";

export const getLocalStatus = () => invoke<LocalStatus>("local_status");
export const getAgentStatus = () => invoke<AgentStatus>("get_status");
export const loadModel = (model: string) => invoke("load_model", { model });
export const restartRuntime = () => invoke("restart_runtime");
export const localUpdate = () => invoke<string | null>("local_update");
export const enroll = (code: string, name: string) => invoke("enroll", { code, name });
export const localChat = (model: string, messages: ChatMsg[]) =>
  invoke<string>("local_chat", { model, messages });
