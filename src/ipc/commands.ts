// The only module that calls `invoke`. Types come from Rust via ts-rs (./bindings).
import { invoke } from "@tauri-apps/api/core";
import type { AgentId } from "./bindings/AgentId";
import type { ClaudeFoldersDto } from "./bindings/ClaudeFoldersDto";
import type { ForgetFolderPreviewDto } from "./bindings/ForgetFolderPreviewDto";
import type { QuotaTapPreviewDto } from "./bindings/QuotaTapPreviewDto";
import type { SkipDefaultFolderPreviewDto } from "./bindings/SkipDefaultFolderPreviewDto";
import type { DashboardDto } from "./bindings/DashboardDto";

export type CommandError = { code: string; message: string };

/** Local time minus UTC, in minutes (e.g. +480 for UTC+8). */
function utcOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

export function getDashboard(): Promise<DashboardDto> {
  return invoke<DashboardDto>("get_dashboard", { utcOffsetMinutes: utcOffsetMinutes() });
}

export function setCollectorEnabled(agent: AgentId, enabled: boolean): Promise<void> {
  return invoke<void>("set_collector_enabled", { agent, enabled });
}

/** Returns how many collectors changed state. */
export function setAllCollectorsEnabled(enabled: boolean): Promise<number> {
  return invoke<number>("set_all_collectors_enabled", { enabled });
}

export function refresh(collectorId?: string): Promise<void> {
  return invoke<void>("refresh", { collectorId: collectorId ?? null });
}

/** Claude Code config folders AgentPulse reads (read-only), plus suggested extra folders. */
export function getClaudeFolders(): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("get_claude_folders");
}

export function addClaudeFolder(path: string): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("add_claude_folder", { path });
}

export function removeClaudeFolder(path: string): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("remove_claude_folder", { path });
}

/** Preview the exact settings.json change for the quota tap. Writes nothing. */
export function previewQuotaTap(folder: string, enable: boolean): Promise<QuotaTapPreviewDto> {
  return invoke<QuotaTapPreviewDto>("preview_quota_tap", { folder, enable });
}

/** Apply a previewed change. Refused if settings.json changed since `preview`. */
export function setQuotaTap(preview: QuotaTapPreviewDto): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("set_quota_tap", {
    folder: preview.folder,
    enable: preview.enable,
    before: preview.before,
  });
}

/** Preview removing a folder that isn't read (e.g. a Team plan) from AgentPulse. Writes nothing. */
export function previewForgetClaudeFolder(folder: string): Promise<ForgetFolderPreviewDto> {
  return invoke<ForgetFolderPreviewDto>("preview_forget_claude_folder", { folder });
}

/** Apply a previewed removal. Refused if settings.json changed since `preview`. */
export function forgetClaudeFolder(preview: ForgetFolderPreviewDto): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("forget_claude_folder", { folder: preview.folder, before: preview.before });
}

/** What turning off the default folder (~/.claude) on this machine would delete from AgentPulse's database. Writes nothing. */
export function previewSkipDefaultClaudeFolder(): Promise<SkipDefaultFolderPreviewDto> {
  return invoke<SkipDefaultFolderPreviewDto>("preview_skip_default_claude_folder");
}

/** Stop reading the default folder on this machine and delete its data from AgentPulse's database. The folder isn't touched. */
export function skipDefaultClaudeFolder(): Promise<ClaudeFoldersDto> {
  return invoke<ClaudeFoldersDto>("skip_default_claude_folder");
}

export function errorMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as CommandError).message);
  return "Something went wrong.";
}
