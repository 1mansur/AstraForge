import type { Bridge } from '../services/bridge';
import type { Methods } from '../domain/types';
type Handlers = { [K in keyof Methods]?: (input: Methods[K]['input']) => Methods[K]['output'] | Promise<Methods[K]['output']> };
export class TestBridge implements Bridge {
  available = true;
  calls: { method: keyof Methods; params: unknown }[] = [];
  events = new Map<string, (payload: unknown) => void>();
  constructor(readonly handlers: Handlers = {}) { this.handlers = { cancel_search: () => null, ...handlers }; }
  async request<K extends keyof Methods>(method: K, params: Methods[K]['input']): Promise<Methods[K]['output']> {
    this.calls.push({ method, params });
    const handler = this.handlers[method] as ((input: Methods[K]['input']) => Methods[K]['output'] | Promise<Methods[K]['output']>) | undefined;
    if (!handler) throw new Error(`Unexpected test request: ${method}`);
    return handler(params);
  }
  async subscribe(event: 'workspace_changed' | 'diagnostic', handler: (payload: unknown) => void): Promise<() => void> { this.events.set(event, handler); return () => { this.events.delete(event); }; }
}
