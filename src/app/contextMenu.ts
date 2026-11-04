// Release builds: suppress WebView2's browser context menu (Refresh, Save as, Print,
// Inspect), which doesn't belong in a desktop app. It stays available where it's useful:
// on selected text (Copy) and in editable fields (Cut/Copy/Paste).
// Dev builds keep the full menu, including Inspect, for debugging.

function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || target.closest("input, textarea, [contenteditable='true']") !== null;
}

function hasTextSelection(): boolean {
  const selection = window.getSelection();
  return selection !== null && !selection.isCollapsed && selection.toString().trim() !== "";
}

export function installReleaseContextMenuGuard(): void {
  if (import.meta.env.DEV) return;
  document.addEventListener("contextmenu", (event) => {
    if (isEditable(event.target) || hasTextSelection()) return;
    event.preventDefault();
  });
}
