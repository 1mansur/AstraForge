import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
export async function watchNativeClose(shouldPrevent: () => boolean, onPrevented: () => void): Promise<() => void> {
  if (!isTauri()) return () => undefined;
  return getCurrentWindow().onCloseRequested(event => { if (shouldPrevent()) { event.preventDefault(); onPrevented(); } });
}
export async function closeNativeWindow(): Promise<void> { if (isTauri()) await getCurrentWindow().destroy(); }
