import { describe, expect, it } from 'vitest';
import { defaultShortcuts, loadShortcuts, matchesShortcut, saveShortcuts } from './shortcuts';
describe('keyboard bindings', () => {
  it('distinguishes palette from file search and supports command on macOS', () => { expect(matchesShortcut(new KeyboardEvent('keydown', { key: 'P', ctrlKey: true, shiftKey: true }), 'mod+shift+p')).toBe(true); expect(matchesShortcut(new KeyboardEvent('keydown', { key: 'p', ctrlKey: true }), 'mod+shift+p')).toBe(false); expect(matchesShortcut(new KeyboardEvent('keydown', { key: 's', metaKey: true }), 'mod+s')).toBe(true); });
  it('persists validated custom bindings and rejects collisions', () => { const bindings = { ...defaultShortcuts, save: 'mod+alt+s' }; saveShortcuts(bindings); expect(loadShortcuts()).toEqual(bindings); expect(() => saveShortcuts({ ...bindings, save: bindings.palette })).toThrow('unique'); });
  it('rejects malformed persisted data', () => { localStorage.setItem('astraforge.shortcuts.v1', '{"save":5}'); expect(() => loadShortcuts()).toThrow('Invalid keyboard'); });
});
