import { useState, type ReactNode } from "react";
import { MetricValue, reasonText } from "../../components/MetricValue";
import { errorMessage, setCollectorEnabled } from "../../ipc/commands";
import type { AgentCardDto } from "../../ipc/bindings/AgentCardDto";
import type { ProviderId } from "../../ipc/bindings/ProviderId";
import type { TokenTotalsDto } from "../../ipc/bindings/TokenTotalsDto";
import { reloadDashboard, setError } from "../../store/engineStore";
import { ClaudeFolders } from "./ClaudeFolders";

const num = new Intl.NumberFormat();
const time = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const resetFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const fmtTokens =(t: TokenTotalsDto) => `${num.format(t.input)} in · ${num.format(t.output)} out`;
const fmtPct = (v: number) => `${v.toFixed(1)}%`;

const PROVIDER: Record<ProviderId, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  google: "Google",
  mock: "Synthetic (dev only)",
};

type Status = { label: string; tone: string; detail?: string };

function status(card: AgentCardDto): Status {
  const c = card.collector;
  if (!c) return { label: "Not available yet", tone: "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400" };
  if (!c.enabled) return { label: "Disabled", tone: "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400" };
  switch (c.status?.state) {
    case "ok":
      return {
        label: "Collecting",
        tone: "bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300",
        detail: c.status.last_success_at ? `Last update ${time.format(new Date(c.status.last_success_at))}` : undefined,
      };
    case "failed":
      return {
        label: "Error",
        tone: "bg-red-100 text-red-800 dark:bg-red-950 dark:text-red-300",
        detail: c.status.last_error ? `${c.status.last_error.code}, retrying (${c.status.consecutive_failures}×)` : undefined,
      };
    case "not_installed":
      return { label: "Not installed", tone: "bg-amber-100 text-amber-800 dark:bg-amber-950 dark:text-amber-300" };
    default:
      return { label: "Starting…", tone: "bg-sky-100 text-sky-800 dark:bg-sky-950 dark:text-sky-300" };
  }
}

export function AgentCard({ card }: { card: AgentCardDto }) {
  const [busy, setBusy] = useState(false);
  const c = card.collector;
  const enabled = c?.enabled ?? false;
  const s = status(card);
  const headingId = `agent-${card.agent}`;

  const toggle = async () => {
    setBusy(true);
    try {
      await setCollectorEnabled(card.agent, !enabled);
      await reloadDashboard();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <article aria-labelledby={headingId} className="flex flex-col gap-4 rounded-xl border border-zinc-200 p-5 dark:border-zinc-800">
      <header className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 id={headingId} className="truncate text-base font-semibold">{card.display_name}</h2>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">{PROVIDER[card.provider]}</p>
        </div>
        <button
          type="button"
          onClick={() => void toggle()}
          disabled={!c || busy}
          aria-pressed={c ? enabled : undefined}
          className={
            "shrink-0 rounded-md px-3 py-1.5 text-sm font-medium focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-40 " +
            (enabled
              ? "border border-zinc-300 hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
              : "bg-zinc-900 text-white hover:bg-zinc-700 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300")
          }
        >
          {busy ? "…" : enabled ? "Disable" : "Enable"}
        </button>
      </header>

      <div className="flex flex-wrap items-center gap-2 text-xs">
        <span className={`rounded-full px-2 py-0.5 font-medium ${s.tone}`}>{s.label}</span>
        {s.detail ? <span className="text-zinc-500 dark:text-zinc-400">{s.detail}</span> : null}
      </div>

      {!c ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400">{card.planned_note}</p>
      ) : !enabled && card.accounts.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400">
          Off. AgentPulse reads nothing from {card.display_name} until you enable it.
        </p>
      ) : (
        <CardBody card={card} />
      )}

      {c && card.agent === "claude_code" ? <ClaudeFolders /> : null}
    </article>
  );
}

function CardBody({ card }: { card: AgentCardDto }) {
  return (
    <div className="flex flex-col gap-4 text-sm">
      {card.accounts.map((a) => (
        <dl key={a.usage_account_id} className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2">
          <dt className="text-zinc-500">Account</dt>
          <dd className="truncate">{a.display_label}</dd>
          <dt className="text-zinc-500">Tokens today</dt>
          <dd><MetricValue metric={a.tokens_today} format={fmtTokens} /></dd>
          {a.quotas.length > 0 ? (
            a.quotas.map((q) => (
              <QuotaRow key={q.window_kind} label={`Quota (${q.window_kind})`}>
                <MetricValue metric={q.used_pct} format={fmtPct} />
                {q.resets_at && q.used_pct.value !== null ? (
                  <span className="ml-2 text-xs text-zinc-500">resets {resetFmt.format(new Date(q.resets_at))}</span>
                ) : null}
              </QuotaRow>
            ))
          ) : (
            <QuotaRow label="Quota">
              <span className="italic text-zinc-500">{reasonText(a.quota_unavailable_reason ?? { kind: "not_yet_collected" })}</span>
            </QuotaRow>
          )}
        </dl>
      ))}

      {card.models_today.length > 0 ? (
        <section aria-label={`${card.display_name} models used today`}>
          <h3 className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">Models today</h3>
          <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
            {card.models_today.map((m) => (
              <li key={m.model} className="flex items-baseline justify-between gap-3 py-1">
                <span className="truncate font-mono text-xs">{m.model}</span>
                <span className="shrink-0 tabular-nums text-xs text-zinc-600 dark:text-zinc-300">
                  {num.format(m.input)} in · {num.format(m.output)} out
                </span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

function QuotaRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-zinc-500">{label}</dt>
      <dd>{children}</dd>
    </>
  );
}
