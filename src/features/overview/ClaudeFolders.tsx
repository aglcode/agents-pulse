// Which Claude Code config folders AgentPulse reads, and the per-folder quota tap.
// Each folder is its own installation (and usually its own Claude account).
// Folders are only ever read, except for the one settings.json change the user
// previews and confirms here.
import { useEffect, useState, type FormEvent } from "react";
import {
  addClaudeFolder,
  errorMessage,
  forgetClaudeFolder,
  getClaudeFolders,
  previewForgetClaudeFolder,
  previewQuotaTap,
  previewSkipDefaultClaudeFolder,
  removeClaudeFolder,
  setQuotaTap,
  skipDefaultClaudeFolder,
} from "../../ipc/commands";
import type { ClaudeFolderDto } from "../../ipc/bindings/ClaudeFolderDto";
import type { ClaudeFolderOrigin } from "../../ipc/bindings/ClaudeFolderOrigin";
import type { ClaudeFoldersDto } from "../../ipc/bindings/ClaudeFoldersDto";
import type { ForgetFolderPreviewDto } from "../../ipc/bindings/ForgetFolderPreviewDto";
import type { QuotaTapPreviewDto } from "../../ipc/bindings/QuotaTapPreviewDto";
import type { SkipDefaultFolderPreviewDto } from "../../ipc/bindings/SkipDefaultFolderPreviewDto";
import { reloadDashboard } from "../../store/engineStore";

const ORIGIN: Record<ClaudeFolderOrigin, string> = {
  default: "default",
  environment: "CLAUDE_CONFIG_DIR",
  added: "added",
};

const buttonClass =
  "shrink-0 rounded-md border border-zinc-300 px-2 py-1 text-xs font-medium hover:bg-zinc-100 focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-800";
const primaryClass =
  "shrink-0 rounded-md bg-zinc-900 px-2 py-1 text-xs font-medium text-white hover:bg-zinc-700 focus-visible:outline-2 focus-visible:outline-offset-2 disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300";

export function ClaudeFolders() {
  const [data, setData] = useState<ClaudeFoldersDto | null>(null);
  const [path, setPath] = useState("");
  const [preview, setPreview] = useState<QuotaTapPreviewDto | null>(null);
  const [forget, setForget] = useState<ForgetFolderPreviewDto | null>(null);
  const [skip, setSkip] = useState<SkipDefaultFolderPreviewDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getClaudeFolders().then(setData, (e) => setError(errorMessage(e)));
  }, []);

  const run = async (op: () => Promise<ClaudeFoldersDto>) => {
    setBusy(true);
    setError(null);
    try {
      setData(await op());
      await reloadDashboard();
      return true;
    } catch (e) {
      setError(errorMessage(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openPreview = async (folder: string, enable: boolean) => {
    setBusy(true);
    setError(null);
    try {
      setPreview(await previewQuotaTap(folder, enable));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const openForget = async (folder: string) => {
    setBusy(true);
    setError(null);
    try {
      setForget(await previewForgetClaudeFolder(folder));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const openSkip = async () => {
    setBusy(true);
    setError(null);
    try {
      setSkip(await previewSkipDefaultClaudeFolder());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const confirmSkip = async () => {
    await run(skipDefaultClaudeFolder);
    setSkip(null);
  };

  const confirmForget = async (p: ForgetFolderPreviewDto) => {
    await run(() => forgetClaudeFolder(p));
    setForget(null);
  };

  const confirm = async (p: QuotaTapPreviewDto) => {
    await run(() => setQuotaTap(p));
    // Close either way: after a refusal (e.g. the file changed) the user reviews a fresh preview.
    setPreview(null);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (path.trim() && (await run(() => addClaudeFolder(path)))) setPath("");
  };

  if (!data) return error ? <p className="text-xs text-red-700 dark:text-red-300">{error}</p> : null;

  return (
    <section aria-labelledby="claude-folders" className="flex flex-col gap-2 text-sm">
      <h3 id="claude-folders" className="text-xs font-medium uppercase tracking-wide text-zinc-500">
        Folders read
      </h3>
      <ul className="flex flex-col gap-2">
        {data.folders.map((f) => (
          <li key={f.path} className="flex flex-col gap-1">
            <div className="flex items-center justify-between gap-2">
              <span className="min-w-0 truncate font-mono text-xs" title={f.path}>
                {f.path}
              </span>
              <span className="flex shrink-0 items-center gap-2 text-[10px] uppercase tracking-wide text-zinc-500">
                {ORIGIN[f.origin]}
                {f.has_logs ? null : <span className="text-amber-700 dark:text-amber-300">no logs</span>}
                {f.origin === "default" ? (
                  <button type="button" className={buttonClass} disabled={busy} onClick={() => void openSkip()}>
                    Don't read on this machine
                  </button>
                ) : null}
                {f.origin === "added" ? (
                  <button type="button" className={buttonClass} disabled={busy} onClick={() => void run(() => removeClaudeFolder(f.path))}>
                    Remove
                  </button>
                ) : null}
              </span>
            </div>
            {f.read ? <QuotaTapRow folder={f} busy={busy} onChange={openPreview} /> : null}
            {f.skipped_reason ? (
              <div className="flex items-center justify-between gap-2 pl-2">
                <span className="text-xs text-zinc-500 dark:text-zinc-400">
                  Not read: {f.skipped_reason}. AgentPulse supports Pro, Max and Ultra only.
                </span>
                <button type="button" className={buttonClass} disabled={busy} onClick={() => void openForget(f.path)}>
                  Remove from AgentPulse
                </button>
              </div>
            ) : null}
            {forget?.folder === f.path ? (
              <ForgetPreview preview={forget} busy={busy} onConfirm={confirmForget} onCancel={() => setForget(null)} />
            ) : null}
            {skip?.folder === f.path ? (
              <SkipPreview preview={skip} busy={busy} onConfirm={confirmSkip} onCancel={() => setSkip(null)} />
            ) : null}
            {preview?.folder === f.path ? (
              <TapPreview preview={preview} busy={busy} onConfirm={confirm} onCancel={() => setPreview(null)} />
            ) : null}
          </li>
        ))}
      </ul>

      {data.suggestions.map((s) => (
        <div key={s} className="flex items-center justify-between gap-2 rounded-md bg-sky-50 px-2 py-1 dark:bg-sky-950">
          <span className="min-w-0 truncate text-xs">
            Found another Claude Code folder: <span className="font-mono">{s}</span>
          </span>
          <button type="button" className={buttonClass} disabled={busy} onClick={() => void run(() => addClaudeFolder(s))}>
            Add
          </button>
        </div>
      ))}

      <form onSubmit={(e) => void submit(e)} className="flex gap-2">
        <label htmlFor="claude-folder-path" className="sr-only">
          Claude Code folder path
        </label>
        <input
          id="claude-folder-path"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder="Add a folder, e.g. ~\.claude-work"
          className="min-w-0 flex-1 rounded-md border border-zinc-300 bg-transparent px-2 py-1 font-mono text-xs dark:border-zinc-700"
        />
        <button type="submit" className={buttonClass} disabled={busy || !path.trim()}>
          Add
        </button>
      </form>
      {error ? (
        <p role="alert" className="text-xs text-red-700 dark:text-red-300">
          {error}
        </p>
      ) : null}
    </section>
  );
}

function QuotaTapRow({
  folder,
  busy,
  onChange,
}: {
  folder: ClaudeFolderDto;
  busy: boolean;
  onChange: (folder: string, enable: boolean) => Promise<void>;
}) {
  const note = (text: string) => <span className="text-xs text-zinc-500 dark:text-zinc-400">{text}</span>;
  return (
    <div className="flex items-center justify-between gap-2 pl-2">
      {folder.quota_tap === "on" ? note("Quota: on (via status line)") : null}
      {folder.quota_tap === "off" ? note("Quota: off. Needs one status line entry in settings.json.") : null}
      {folder.quota_tap === "other_status_line" ? note("Quota: off. This folder has its own status line, which AgentPulse leaves alone.") : null}
      {folder.quota_tap === "unreadable" ? note("Quota: off. settings.json isn't valid JSON.") : null}
      {folder.quota_tap === "unsupported" ? note("Quota: unavailable for this AgentPulse install.") : null}
      {folder.quota_tap === "plan_without_quota"
        ? note("Quota: not offered. Claude Code reports quota only for Pro, Max and Ultra plans.")
        : null}
      {folder.quota_tap === "on" || folder.quota_tap === "off" ? (
        <button
          type="button"
          className={buttonClass}
          disabled={busy}
          onClick={() => void onChange(folder.path, folder.quota_tap === "off")}
        >
          {folder.quota_tap === "on" ? "Disable quota" : "Enable quota"}
        </button>
      ) : null}
    </div>
  );
}

function TapPreview({
  preview,
  busy,
  onConfirm,
  onCancel,
}: {
  preview: QuotaTapPreviewDto;
  busy: boolean;
  onConfirm: (p: QuotaTapPreviewDto) => Promise<void>;
  onCancel: () => void;
}) {
  return (
    <div role="region" aria-label="Review settings change" className="flex flex-col gap-2 rounded-md border border-amber-400 p-3 text-xs dark:border-amber-600">
      <p>
        {preview.enable ? "AgentPulse will add these lines to " : "AgentPulse will remove these lines from "}
        <span className="font-mono">{preview.settings_path}</span>:
      </p>
      <pre className="overflow-x-auto rounded bg-zinc-100 p-2 font-mono text-[11px] dark:bg-zinc-900">{preview.block}</pre>
      <p className="text-zinc-500 dark:text-zinc-400">
        {preview.enable
          ? "Nothing else in the file changes, and your chats and login aren't touched. Claude Code will run AgentPulse as its status line and show quota at the bottom. Disable removes exactly these lines."
          : "Nothing else in the file changes."}
      </p>
      <div className="flex justify-end gap-2">
        <button type="button" className={buttonClass} disabled={busy} onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className={primaryClass} disabled={busy} onClick={() => void onConfirm(preview)}>
          {preview.enable ? "Add to settings.json" : "Remove from settings.json"}
        </button>
      </div>
    </div>
  );
}

function ForgetPreview({
  preview,
  busy,
  onConfirm,
  onCancel,
}: {
  preview: ForgetFolderPreviewDto;
  busy: boolean;
  onConfirm: (p: ForgetFolderPreviewDto) => Promise<void>;
  onCancel: () => void;
}) {
  return (
    <div role="region" aria-label="Review removal" className="flex flex-col gap-2 rounded-md border border-amber-400 p-3 text-xs dark:border-amber-600">
      <p>AgentPulse will:</p>
      <ul className="list-disc pl-5">
        {preview.accounts.length > 0 ? (
          <li>
            Delete its stored data for {preview.accounts.join(", ")} (tokens, sessions, quota history) from AgentPulse's own database.
          </li>
        ) : (
          <li>Delete nothing from its database (no data is stored for this folder).</li>
        )}
        {preview.tap_block ? (
          <li>
            Remove these lines from <span className="font-mono">{preview.settings_path}</span>:
            <pre className="mt-1 overflow-x-auto rounded bg-zinc-100 p-2 font-mono text-[11px] dark:bg-zinc-900">{preview.tap_block}</pre>
          </li>
        ) : null}
        <li>Stop reading this folder. Your chats, login and everything else in it stay as they are.</li>
      </ul>
      <div className="flex justify-end gap-2">
        <button type="button" className={buttonClass} disabled={busy} onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className={primaryClass} disabled={busy} onClick={() => void onConfirm(preview)}>
          Remove
        </button>
      </div>
    </div>
  );
}

function SkipPreview({
  preview,
  busy,
  onConfirm,
  onCancel,
}: {
  preview: SkipDefaultFolderPreviewDto;
  busy: boolean;
  onConfirm: () => Promise<void>;
  onCancel: () => void;
}) {
  const n = (v: number) => v.toLocaleString();
  return (
    <div role="region" aria-label="Review turning off the default folder" className="flex flex-col gap-2 rounded-md border border-amber-400 p-3 text-xs dark:border-amber-600">
      <p>On this machine, AgentPulse will:</p>
      <ul className="list-disc pl-5">
        <li>
          Stop reading and watching <span className="font-mono">{preview.folder}</span>.
        </li>
        {preview.sessions > 0 ? (
          <li>
            Delete its summaries of the {n(preview.sessions)} sessions that came from it ({n(preview.records)} usage
            records, {n(preview.tokens)} input + output tokens) from its own database.
            {preview.accounts.length > 0 ? <> The account ({preview.accounts.join(", ")}) and its data from your other folders stay.</> : <> Data from your other folders stays.</>}
          </li>
        ) : (
          <li>Delete nothing from its database (no sessions are stored for this folder).</li>
        )}
      </ul>
      <p className="text-zinc-500 dark:text-zinc-400">
        Nothing in the folder changes: your chats, login and settings stay as they are. Other computers keep reading
        their own default folder. To read it again later, add it back below (<span className="font-mono">~\.claude</span>).
      </p>
      <div className="flex justify-end gap-2">
        <button type="button" className={buttonClass} disabled={busy} onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className={primaryClass} disabled={busy} onClick={() => void onConfirm()}>
          Stop reading
        </button>
      </div>
    </div>
  );
}
