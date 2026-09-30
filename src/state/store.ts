export class Store<T> {
  private listeners = new Set<() => void>();
  constructor(private value: T) {}
  get = (): T => this.value;
  subscribe = (listener: () => void): (() => void) => { this.listeners.add(listener); return () => this.listeners.delete(listener); };
  set(value: T | ((previous: T) => T)): void { this.value = typeof value === 'function' ? (value as (previous: T) => T)(this.value) : value; this.listeners.forEach(listener => listener()); }
  patch(patch: Partial<T>): void { this.set(previous => ({ ...previous, ...patch })); }
}
