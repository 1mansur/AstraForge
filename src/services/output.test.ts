import { describe, expect, it } from 'vitest';
import type { ProcessSession } from '../domain/types';
import { appendOutput, outputCharacterLimit, outputChunkLimit } from './output';
const base: ProcessSession = { id: 'process', label: 'test', spec: { program: 'test', args: [], cwd: null, env: {}, approved: true, isTest: true }, chunks: [], cursor: 0, running: true, drained: false, exitCode: null, truncated: false, startedAt: 0 };
describe('bounded process rendering state', () => {
  it('bounds fragmented output across hundreds of separate poll responses', () => {
    let session = base; const started = performance.now();
    for (let poll = 0; poll < 500; poll++) { const chunks = Array.from({ length: 1000 }, (_, index) => ({ sequence: poll * 1000 + index + 1, stream: index % 2 ? 'stderr' : 'stdout', text: 'x' })); session = appendOutput(session, { id: base.id, chunks, nextCursor: (poll + 1) * 1000, running: true, exitCode: null, truncated: false }); expect(session.chunks.length).toBeLessThanOrEqual(outputChunkLimit); }
    process.stdout.write(`${JSON.stringify({ profile: 'fragmented-output', polls: 500, receivedChunks: 500000, retainedChunks: session.chunks.length, processingMs: performance.now() - started })}\n`); expect(session.cursor).toBe(500000); expect(session.chunks.at(-1)?.sequence).toBe(500000); expect(session.truncated).toBe(true);
  });
  it('rejects duplicated, reordered or mismatched snapshots without mutating retained state', () => { const session = appendOutput(base, { id: base.id, chunks: [{ sequence: 1, stream: 'stderr', text: 'saved' }], nextCursor: 1, running: true, exitCode: null, truncated: false }); for (const result of [{ id: 'other', chunks: [], nextCursor: 1 }, { id: base.id, chunks: [], nextCursor: 0 }, { id: base.id, chunks: [{ sequence: 1, stream: 'stdout', text: 'duplicate' }], nextCursor: 1 }]) expect(() => appendOutput(session, { ...result, running: true, exitCode: null, truncated: false })).toThrow('Invalid process output'); expect(session.chunks[0]?.text).toBe('saved'); });
  it('bounds a huge chunk and drains output after process termination', () => { const tail = appendOutput(base, { id: base.id, chunks: [{ sequence: 1, stream: 'stdout', text: 'x'.repeat(outputCharacterLimit + 50) }], nextCursor: 1, running: false, exitCode: 0, truncated: false }); expect(tail.chunks[0]?.text.length).toBe(outputCharacterLimit); expect(tail.drained).toBe(false); expect(appendOutput(tail, { id: base.id, chunks: [], nextCursor: 1, running: false, exitCode: 0, truncated: false }).drained).toBe(true); });
});
