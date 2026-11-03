// Renders any Metric<T> truthfully: value + source + freshness, or the reason it
// is unavailable. Never renders an unavailable metric as 0.
import type { Freshness } from "../ipc/bindings/Freshness";
import type { Metric } from "../ipc/bindings/Metric";
import type { SourceKind } from "../ipc/bindings/SourceKind";
import type { Stability } from "../ipc/bindings/Stability";
import type { UnavailableReason } from "../ipc/bindings/UnavailableReason";

const SOURCE_LABEL: Record<SourceKind, string> = {
  official_api: "Official API",
  official_telemetry: "Official telemetry",
  local: "Local",
  estimated: "Estimated",
  unavailable: "Unavailable",
};

export function reasonText(r: UnavailableReason | null): string {
  switch (r?.kind) {
    case "not_exposed_by_provider": return "Not exposed by provider";
    case "provider_not_installed": return "Provider not installed";
    case "provider_not_running": return "Provider not running";
    case "not_authenticated": return "Not signed in";
    case "unsupported_version": return `Unsupported version ${r.detected}`;
    case "opt_in_required": return "Requires opt-in in Settings";
    case "collector_disabled": return "Collector disabled";
    case "collection_failed": return "Collection failed";
    case "not_yet_collected": return "Not collected yet";
    default: return "Unavailable";
  }
}

export function SourceBadge({ source, stability }: { source: SourceKind; stability: Stability | null }) {
  const unofficial = source === "local" && stability === "undocumented";
  const tone =
    source === "estimated"
      ? "border-dashed border-amber-500 text-amber-700 dark:text-amber-300"
      : unofficial
        ? "border-zinc-400 text-zinc-600 dark:text-zinc-300"
        : "border-emerald-500 text-emerald-700 dark:text-emerald-300";
  return (
    <span className={`rounded border px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide ${tone}`}>
      {SOURCE_LABEL[source]}
      {unofficial ? " · unofficial" : ""}
    </span>
  );
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });

export function FreshnessTag({ freshness, observedAt }: { freshness: Freshness; observedAt: number | null }) {
  if (freshness.state !== "stale") return null;
  const at = observedAt ? timeFmt.format(new Date(observedAt)) : "unknown time";
  return <span className="text-[10px] font-medium text-amber-700 dark:text-amber-300">as of {at} · stale</span>;
}

export function MetricValue<T>({ metric, format }: { metric: Metric<T>; format: (v: T) => string }) {
  if (metric.value === null) {
    return <span className="text-sm italic text-zinc-500 dark:text-zinc-400">{reasonText(metric.unavailable_reason)}</span>;
  }
  const stale = metric.freshness.state === "stale";
  return (
    <span className="inline-flex flex-wrap items-center gap-2" title={metric.estimation_basis ?? undefined}>
      <span className={`font-semibold tabular-nums ${stale ? "opacity-50" : ""}`}>{format(metric.value)}</span>
      <SourceBadge source={metric.source} stability={metric.stability} />
      <FreshnessTag freshness={metric.freshness} observedAt={metric.observed_at} />
      {metric.last_error ? (
        <span className="text-[10px] text-red-600 dark:text-red-400" title={metric.last_error.message}>
          last run failed
        </span>
      ) : null}
    </span>
  );
}
