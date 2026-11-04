// Minimal external store (useSyncExternalStore) with per-slice selectors, so a
// change to one slice only re-renders components that select it.
import { useSyncExternalStore } from "react";
import { errorMessage, getDashboard } from "../ipc/commands";
import { onCollectorStatus, onDataChanged, onFreshnessTick, onResync } from "../ipc/events";
import type { DashboardDto } from "../ipc/bindings/DashboardDto";

type EngineState = {
  dashboard: DashboardDto | null;
  error: string | null;
};

let state: EngineState = { dashboard: null, error: null };
const listeners = new Set<() => void>();

function setState(patch: Partial<EngineState>) {
  state = { ...state, ...patch };
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Select a slice. Selectors must return existing references (no new objects). */
export function useEngine<T>(selector: (s: EngineState) => T): T {
  return useSyncExternalStore(subscribe, () => selector(state));
}

// Coalesce overlapping reloads: at most one request in flight plus one queued.
let inFlight: Promise<void> | null = null;
let queued = false;

export function reloadDashboard(): Promise<void> {
  if (inFlight) {
    queued = true;
    return inFlight;
  }
  inFlight = (async () => {
    try {
      setState({ dashboard: await getDashboard(), error: null });
    } catch (e) {
      setState({ error: errorMessage(e) });
    } finally {
      inFlight = null;
      if (queued) {
        queued = false;
        void reloadDashboard();
      }
    }
  })();
  return inFlight;
}

export function setError(message: string | null) {
  setState({ error: message });
}

/** Initial load + live updates. Returns a cleanup function. */
export function startEngineSync(): () => void {
  let disposed = false;
  const unlisteners: Array<() => void> = [];
  const track = (p: Promise<() => void>) =>
    p.then((un) => (disposed ? un() : unlisteners.push(un))).catch(() => {});

  void reloadDashboard();
  track(onDataChanged(() => void reloadDashboard()));
  track(onCollectorStatus(() => void reloadDashboard()));
  track(onResync(() => void reloadDashboard()));
  // Freshness is computed when data is read. The backend sends this tick only
  // while the window is visible, so values age into "stale" without new data.
  track(onFreshnessTick(() => void reloadDashboard()));

  return () => {
    disposed = true;
    unlisteners.forEach((un) => un());
  };
}
