export type ShortcutAction = 'palette' | 'files' | 'search' | 'save' | 'terminal' | 'agent' | 'explorer' | 'settings';
export type ShortcutBindings = Record<ShortcutAction, string>;
export const defaultShortcuts: ShortcutBindings = { palette: 'mod+shift+p', files: 'mod+p', search: 'mod+shift+f', save: 'mod+s', terminal: 'mod+`', agent: 'mod+shift+a', explorer: 'mod+b', settings: 'mod+,' };
export function matchesShortcut(event: KeyboardEvent, binding: string): boolean {
  const keys = binding.toLowerCase().split('+');
  const key = keys.at(-1);
  return Boolean(key && event.key.toLowerCase() === key && (event.ctrlKey || event.metaKey) === keys.includes('mod') && event.shiftKey === keys.includes('shift') && event.altKey === keys.includes('alt'));
}
export function loadShortcuts(): ShortcutBindings {
  const saved = localStorage.getItem('astraforge.shortcuts.v1');
  if (!saved) return { ...defaultShortcuts };
  const parsed: unknown = JSON.parse(saved);
  if (typeof parsed !== 'object' || parsed === null) throw new Error('Stored keyboard shortcuts are invalid. Restore defaults in Settings.');
  const result = { ...defaultShortcuts };
  for (const action of Object.keys(defaultShortcuts) as ShortcutAction[]) { if (action in parsed) { const value = Reflect.get(parsed, action); if (typeof value !== 'string' || !/^(mod\+)?(shift\+)?(alt\+)?[^+]+$/.test(value)) throw new Error(`Invalid keyboard shortcut for ${action}.`); result[action] = value; } }
  return result;
}
export function saveShortcuts(bindings: ShortcutBindings): void {
  if (new Set(Object.values(bindings)).size !== Object.keys(bindings).length) throw new Error('Keyboard shortcuts must be unique.');
  for (const binding of Object.values(bindings)) { if (!/^(mod\+)(shift\+)?(alt\+)?[^+]+$/.test(binding)) throw new Error('Shortcuts use mod+[shift+][alt+]key, for example mod+shift+p.'); }
  localStorage.setItem('astraforge.shortcuts.v1', JSON.stringify(bindings));
}
