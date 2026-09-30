import { describe, expect, it } from 'vitest';
import { decodeEvent } from './events';
const envelope = { version: 1, repositoryId: 'repository', sequence: 1 };
describe('versioned native event decoder', () => {
  it('accepts the native event envelope and deduplicates paths', () => { expect(decodeEvent('workspace_changed', { ...envelope, payload: { paths: ['src/雪.ts', 'src/雪.ts'], rescan: false } })).toEqual({ ...envelope, payload: { paths: ['src/雪.ts'], rescan: false } }); expect(decodeEvent('diagnostic', { ...envelope, repositoryId: null, payload: { message: 'Failure', subsystem: 'index' } })?.payload.message).toBe('Failure'); });
  it.each([null, [], false, 'event', {}, { paths: ['a'], rescan: true }, { ...envelope, version: 2, payload: { paths: [], rescan: true } }, { ...envelope, sequence: -1, payload: { paths: [], rescan: true } }, { ...envelope, sequence: Number.MAX_SAFE_INTEGER + 1, payload: { paths: [], rescan: true } }, { ...envelope, payload: { paths: ['../outside'], rescan: false } }, { ...envelope, payload: { paths: [12], rescan: false } }, { ...envelope, payload: { paths: [], rescan: 'true' } }])('rejects malformed or incompatible events without throwing: %j', value => { expect(decodeEvent('workspace_changed', value)).toBeNull(); });
  it('turns oversized or root invalidations into bounded full reconciliations', () => { for (const paths of [Array.from({ length: 5000 }, (_, index) => `${index}.ts`), [''], ['x'.repeat(5000)]]) expect(decodeEvent('workspace_changed', { ...envelope, payload: { paths, rescan: false } })?.payload).toEqual({ paths: [], rescan: true }); });
  it('fails safely for deterministic generated JSON values', () => {
    let seed = 739391; const next = () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed; }; const values: unknown[] = [null, false, 3, '', {}, [], { nested: [] }];
    for (let index = 0; index < 2000; index++) { const value = { version: values[next() % values.length], sequence: values[next() % values.length], repositoryId: values[next() % values.length], payload: { paths: values[next() % values.length], rescan: values[next() % values.length] } }; expect(decodeEvent('workspace_changed', JSON.parse(JSON.stringify(value)) as unknown)).toBeNull(); }
  });
});
