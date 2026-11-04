// Overview: one card per AI tool, each with its own Enable/Disable, plus Enable all.
import { useState } from "react";
import { errorMessage, setAllCollectorsEnabled } from "../../ipc/commands";
import { reloadDashboard, setError, useEngine } from "../../store/engineStore";
import { AgentCard } from "./AgentCard";

export function Overview() {
  const dashboard = useEngine((s) => s.dashboard);
  const error = useEngine((s) => s.error);
  const [busy, setBusy] = useState(false);

  const cards = dashboard?.agents ?? [];
  const available = cards.filter((c) => c.collector !== null);
  const allEnabled = available.length > 0 && available.every((c) => c.collector?.enabled);
  const noneEnabled = available.every((c) => !c.collector?.enabled);
  const hasMock = cards.some((c) => c.agent === "mock");

  const toggleAll = async () => {
    setBusy(true);
    try {
      await setAllCollectorsEnabled(!allEnabled);
      await reloadDashboard();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="mx-auto flex max-w-5xl flex-col gap-6 p-6">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">AgentPulse</h1>
          <p className="text-sm text-zinc-500 dark:text-zinc-400">Overview</p>
        </div>
        <button
          type="button"
          onClick={() => void toggleAll()}
          disabled={busy || available.length === 0}
          title={available.length === 0 ? "No collectors are available in this build yet" : undefined}
          className={
            "rounded-md px-4 py-2 text-sm font-medium focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-40 " +
            (allEnabled
              ? "border border-zinc-300 hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
              : "bg-zinc-900 text-white hover:bg-zinc-700 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300")
          }
        >
          {busy ? "Working…" : allEnabled ? "Disable all" : "Enable all"}
        </button>
      </header>

      {hasMock ? (
        <p role="note" className="rounded-md border border-amber-400 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:bg-amber-950 dark:text-amber-200">
          Development build: the Mock card produces synthetic data to exercise the pipeline. It is not real usage.
        </p>
      ) : null}
      {dashboard && noneEnabled ? (
        <p className="text-sm text-zinc-600 dark:text-zinc-400">
          All collectors are off. AgentPulse reads nothing from your AI tools until you enable them.
        </p>
      ) : null}
      {error ? <p role="alert" className="text-sm text-red-600 dark:text-red-400">{error}</p> : null}

      {dashboard === null ? (
        <p className="text-sm text-zinc-500">Loading…</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          {cards.map((card) => (
            <AgentCard key={card.agent} card={card} />
          ))}
        </div>
      )}
    </main>
  );
}
