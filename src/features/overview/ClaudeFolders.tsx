// Which Claude Code config folders AgentPulse reads. Each folder is its own
// installation (and usually its own Claude account). Folders are only ever read.
import { useEffect, useState, type FormEvent } from "react";
import { addClaudeFolder, errorMessage, getClaudeFolders, removeClaudeFolder } from "../../ipc/commands";
import type { ClaudeFolderOrigin } from "../../ipc/bindings/ClaudeFolderOrigin";
import type { ClaudeFoldersDto } from "../../ipc/bindings/ClaudeFoldersDto";
import { reloadDashboard } from "../../store/engineStore";

const ORIGIN: Record<ClaudeFolderOrigin, string> = {
  default: "default",
  environment: "CLAUDE_CONFIG_DIR",
  added: "added",
};

const buttonClass =
  "shrink-0 rounded-md border border-zinc-300 px-2 py-1 text-xs font-medium hover:bg-zinc-100 focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-800";

export function ClaudeFolders() {
  const [data, setData] = useState<ClaudeFoldersDto | null>(null);
  const [path, setPath] = useState("");
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

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (path.trim() && (await run(() => addClaudeFolder(path)))) setPath("");
  };

  if (!data) return error ? <p className="text-xs text-red-700 dark:text-red-300">{error}</p> : null;

  return (
    <section aria-labelledby="claude-folders" className="flex flex-col gap-2 text-sm">
      <h3 id="claude-folders" className="text-xs font-medium uppercase tracking-wide text-zinc-500">
        Folders read (read-only)
      </h3>
      <ul className="flex flex-col gap-1">
        {data.folders.map((f) => (
          <li key={f.path} className="flex items-center justify-between gap-2">
            <span className="min-w-0 truncate font-mono text-xs" title={f.path}>
              {f.path}
            </span>
            <span className="flex shrink-0 items-center gap-2 text-[10px] uppercase tracking-wide text-zinc-500">
              {ORIGIN[f.origin]}
              {f.has_logs ? null : <span className="text-amber-700 dark:text-amber-300">no logs</span>}
              {f.origin === "added" ? (
                <button type="button" className={buttonClass} disabled={busy} onClick={() => void run(() => removeClaudeFolder(f.path))}>
                  Remove
                </button>
              ) : null}
            </span>
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
