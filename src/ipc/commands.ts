// The only module that calls `invoke`. Types come from Rust via ts-rs (./bindings).
import { invoke } from "@tauri-apps/api/core";
import type { AgentId } from "./bindings/AgentId";
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

export function errorMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as CommandError).message);
  return "Something went wrong.";
}
