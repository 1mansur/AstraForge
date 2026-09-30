import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { AppError, Methods } from '../domain/types';
export interface Bridge {
  available: boolean;
  request<K extends keyof Methods>(method: K, params: Methods[K]['input']): Promise<Methods[K]['output']>;
  subscribe(event: 'workspace_changed' | 'diagnostic', handler: (payload: unknown) => void): Promise<() => void>;
}
export function normalizeError(error: unknown): AppError {
  if (typeof error === 'object' && error !== null && 'message' in error && typeof error.message === 'string') {
    return { code: 'code' in error && typeof error.code === 'string' ? error.code : 'APPLICATION_ERROR', message: error.message, category: 'category' in error && typeof error.category === 'string' ? error.category : 'application', recoverable: 'recoverable' in error ? error.recoverable === true : true, context: null };
  }
  return { code: 'APPLICATION_ERROR', message: typeof error === 'string' ? error : 'The operation could not be completed.', category: 'application', recoverable: true, context: null };
}
export const desktopBridge: Bridge = {
  available: isTauri(),
  async request(method, params) {
    if (!isTauri()) throw normalizeError({ code: 'DESKTOP_REQUIRED', message: 'Launch the AstraForge desktop application to access local repositories.' });
    try { return await invoke('request', { request: { method, params } }); } catch (error) { throw normalizeError(error); }
  },
  async subscribe(event, handler) {
    if (!isTauri()) return () => undefined;
    return listen(event, ({ payload }) => handler(payload));
  },
};
