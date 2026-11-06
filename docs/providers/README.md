# Provider verification (M2)

What each AI tool **actually** exposes on this machine, checked against the installed versions on 2026-11-04. Per-tool details are in [`claude_code.md`](claude_code.md), [`codex.md`](codex.md), [`gemini_cli.md`](gemini_cli.md) and [`antigravity.md`](antigravity.md).

**Method:** read-only.
- Logs and settings were inspected for **key names and value types only**; no prompts, responses or file contents were read.
- No credential file was opened.
- **No live calls were made to any local service.** Those are listed below as pending approvals.

## Summary

| | Claude Code 2.1.318 | Codex 0.157.0 | Gemini CLI 0.40.0 | Antigravity IDE 2.5.5 |
|---|---|---|---|---|
| **Tokens** | ✅ local JSONL (undocumented) | ✅ local rollouts (undocumented) | ✅ official telemetry *(opt-in config)* | ❌ no source found |
| **Quota** | ✅ status line, documented *(opt-in config)* | ✅ rollouts, last seen · live via app-server ⏳ | ❌ internal API only | ⚠️ undocumented LS API ⏳ |
| **Context** | ✅ status line *(opt-in)* · or JSONL + estimated total | ✅ rollouts | used: telemetry · total: estimated | ⚠️ total only ⏳ |
| **Identity** | ✅ `claude auth status --json` (official CLI) | ✅ app-server `account/read` ⏳ · or install-scoped | ✅ telemetry / `google_accounts.json` | ⏳ install-scoped until verified |
| **Works with no config change** | tokens, identity | **everything except live quota** | identity only | — |
| **Needs your approval** | edit `~/.claude/settings.json` (status line tap) | run `codex app-server` (contacts OpenAI via your client) | edit `~/.gemini/settings.json` (telemetry) | read LS process args (CSRF token) + live probe |

## Findings that change the plan
1. **Antigravity's discovery mechanism changed.** All daemon files are stale (Feb 2026, LS 1.19.4, dead PIDs). Version 2.5.5 appears to pass its CSRF token on the process command line. Antigravity also offers **no token usage** source. It's the weakest and riskiest integration, not the strongest.
2. **Codex is the strongest.** Tokens, 5h/7d quota, context and plan are all in local rollouts with zero config changes. A supported interface (`app-server account/read`) provides a **verified email**, so the earlier "install-scoped only" decision for Codex (architecture §6.3) can be upgraded without touching `auth.json`.
3. **Claude Code's quota and context come from a documented interface (the status line)**, but only through an opt-in hook in the user's Claude settings.
4. **Gemini CLI quota is not available** through any supported means. Telemetry logs prompts by default, so AgentPulse must force `logPrompts: false`.
5. **Context totals are Estimated** wherever a tool doesn't report its window (Claude JSONL, Gemini).

## Collector order: approved 2026-11-04 (replaces architecture §11 M3–M6)

The integrations in "Decisions needed" item 2 are deferred to each tool's own milestone. M3 (Codex) uses local logs only.

| Milestone | Tool | Why |
|---|---|---|
| M3 | **Codex** | Richest data, no config changes, verified offline |
| M4 | **Claude Code** | Tokens and identity with no changes; quota and context via an opt-in status line tap |
| M5 | **Gemini CLI** | Needs an opt-in telemetry change; tokens only |
| M6 | **Antigravity** | Only after a live probe is approved and the discovery mechanism is verified |

## Read-only rule (decided 2026-11-06)
**AgentPulse only reads AI tools' folders; it never writes to, reconfigures or runs them in a way that changes their files.** This settles two of the decisions below:
- **2(b) Claude status line tap: approved later the same day as an opt-in button (M4b).** It's the one exception to the rule: a per-folder **Enable quota** button adds a single `statusLine` block to that folder's `settings.json`, only after the user sees and confirms the exact lines. **Disable quota** removes exactly those lines. Until it's enabled, quota stays Unavailable (`requires_tool_settings_change`).
- **2(c) Gemini telemetry edit: rejected.** M5 can use telemetry only if the user has already turned it on.

It also changed Claude Code identity: `claude auth status` writes to the config folder when it runs, so M4 reads `.claude.json` instead (`claude_code.md`).

Still open: **2(a)** Codex `app-server` (it must first be verified not to write to `~/.codex`), and **2(d)** the Antigravity probe.

## Decisions needed
1. **Approve the reorder above**, or keep Antigravity first. Keeping it first needs decision 2(d) right away.
2. **Approve, per item:**
   - (a) **Codex `app-server`**: run Codex's own app-server to read identity (`account/read`, `refreshToken: false`) and live rate limits (rarely, ≥ 5 min). One live verification call is needed before M3 relies on it.
   - (b) **Claude status line tap** (M4): an opt-in, reversible edit to `~/.claude/settings.json`.
   - (c) **Gemini telemetry** (M5): an opt-in, reversible edit to `~/.gemini/settings.json` with `logPrompts: false`.
   - (d) **Antigravity live probe** (M6): read the LS process command line for the CSRF token (memory only), plus one `GetUserStatus` call recording only the response shape.
3. **Architecture updates** that follow, to be applied when the relevant milestone starts:
   - §6.1: Claude identity from `claude auth status --json` (VerifiedEmail + org) instead of `~/.claude.json`.
   - §6.3: Codex identity via `account/read` if 2(a) is approved.
   - §7: Antigravity discovery via the process, not daemon files.
