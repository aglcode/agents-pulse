// The only module that calls `listen`. Event names mirror src-tauri/src/events.rs.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DataChangedPayload } from "./bindings/DataChangedPayload";

export const DATA_CHANGED = "engine://data-changed";
export const COLLECTOR_STATUS = "engine://collector-status";
export const RESYNC = "engine://resync";
export const FRESHNESS_TICK = "engine://freshness-tick";

export function onDataChanged(cb: (p: DataChangedPayload) => void): Promise<UnlistenFn> {
  return listen<DataChangedPayload>(DATA_CHANGED, (e) => cb(e.payload));
}

export function onCollectorStatus(cb: () => void): Promise<UnlistenFn> {
  return listen(COLLECTOR_STATUS, () => cb());
}

export function onResync(cb: () => void): Promise<UnlistenFn> {
  return listen(RESYNC, () => cb());
}

/** Sent by the backend only while the window is visible. */
export function onFreshnessTick(cb: () => void): Promise<UnlistenFn> {
  return listen(FRESHNESS_TICK, () => cb());
}
