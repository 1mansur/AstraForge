import type { OutputChunk, ProcessSession, ProcessSnapshot } from '../domain/types';
export const outputCharacterLimit = 500000;
export const outputChunkLimit = 2048;
export const processSessionLimit = 128;
export function appendOutput(session: ProcessSession, result: ProcessSnapshot): ProcessSession {
  if (result.id !== session.id || !Number.isSafeInteger(result.nextCursor) || result.nextCursor < session.cursor) throw new Error('Invalid process output cursor.');
  const incoming: OutputChunk[] = []; let sequence = session.cursor;
  for (const chunk of result.chunks) {
    if (!Number.isSafeInteger(chunk.sequence) || chunk.sequence <= sequence || chunk.sequence > result.nextCursor || typeof chunk.text !== 'string' || !['stdout', 'stderr'].includes(chunk.stream)) throw new Error('Invalid process output sequence.');
    sequence = chunk.sequence; incoming.push(chunk);
  }
  const chunks = [...session.chunks, ...incoming]; let length = chunks.reduce((sum, chunk) => sum + chunk.text.length, 0); let start = 0; let truncated = session.truncated || result.truncated;
  while ((length > outputCharacterLimit || chunks.length - start > outputChunkLimit) && start < chunks.length - 1) { length -= chunks[start]?.text.length ?? 0; start++; truncated = true; }
  const retained = chunks.slice(start);
  if (retained[0] && retained[0].text.length > outputCharacterLimit) { retained[0] = { ...retained[0], text: retained[0].text.slice(-outputCharacterLimit) }; truncated = true; }
  return { ...session, chunks: retained, cursor: result.nextCursor, running: result.running, drained: !result.running && result.chunks.length === 0, exitCode: result.exitCode, truncated };
}
