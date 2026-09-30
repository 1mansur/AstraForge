export interface EventEnvelope { version: 1; repositoryId: string | null; sequence: number }
export interface WorkspaceEvent extends EventEnvelope { payload: { paths: string[]; rescan: boolean } }
export interface DiagnosticEvent extends EventEnvelope { payload: { message: string } }
function record(value: unknown): value is Record<string, unknown> { return typeof value === 'object' && value !== null && !Array.isArray(value); }
function envelope(value: unknown): value is Record<string, unknown> & EventEnvelope {
  return record(value) && value.version === 1 && (value.repositoryId === null || typeof value.repositoryId === 'string' && value.repositoryId.length > 0 && value.repositoryId.length <= 128) && typeof value.sequence === 'number' && Number.isSafeInteger(value.sequence) && value.sequence > 0;
}
export function decodeEvent(name: 'workspace_changed', value: unknown): WorkspaceEvent | null;
export function decodeEvent(name: 'diagnostic', value: unknown): DiagnosticEvent | null;
export function decodeEvent(name: 'workspace_changed' | 'diagnostic', value: unknown): WorkspaceEvent | DiagnosticEvent | null;
export function decodeEvent(name: 'workspace_changed' | 'diagnostic', value: unknown): WorkspaceEvent | DiagnosticEvent | null {
  if (!envelope(value) || !record(value.payload)) return null;
  const base: EventEnvelope = { version: 1, repositoryId: value.repositoryId, sequence: value.sequence };
  if (name === 'diagnostic') return typeof value.payload.message === 'string' && value.payload.message.length <= 8192 ? { ...base, payload: { message: value.payload.message } } : null;
  if (!value.repositoryId || !Array.isArray(value.payload.paths) || typeof value.payload.rescan !== 'boolean') return null;
  if (value.payload.paths.length > 4096) return { ...base, payload: { paths: [], rescan: true } };
  const paths: string[] = []; let length = 0;
  for (const path of value.payload.paths) {
    if (typeof path !== 'string' || path.includes('\0') || path.startsWith('/') || path.includes('\\') || path.split('/').some(part => part === '..')) return null;
    length += path.length;
    if (!path || path.length > 4096 || length > 262144) return { ...base, payload: { paths: [], rescan: true } };
    paths.push(path);
  }
  return { ...base, payload: { paths: [...new Set(paths)], rescan: value.payload.rescan } };
}
