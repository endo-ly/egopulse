/**
 * Tracks forms with unsaved edits so navigation and page unload can warn
 * before discarding them.
 */
const dirtyForms = new Set<string>();

function onBeforeUnload(event: BeforeUnloadEvent) {
  event.preventDefault();
}

export function setUnsavedChanges(formId: string, dirty: boolean): void {
  const before = dirtyForms.size;
  if (dirty) dirtyForms.add(formId);
  else dirtyForms.delete(formId);
  if (before === 0 && dirtyForms.size > 0) globalThis.addEventListener?.("beforeunload", onBeforeUnload);
  if (before > 0 && dirtyForms.size === 0) globalThis.removeEventListener?.("beforeunload", onBeforeUnload);
}

/** Asks before leaving unsaved edits; true when it is fine to navigate. */
export function confirmDiscardUnsaved(): boolean {
  if (dirtyForms.size === 0) return true;
  const ok = globalThis.confirm?.("You have unsaved config changes. Discard them?") ?? true;
  if (ok) {
    dirtyForms.clear();
    globalThis.removeEventListener?.("beforeunload", onBeforeUnload);
  }
  return ok;
}
