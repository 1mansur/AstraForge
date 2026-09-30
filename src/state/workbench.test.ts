import { describe, expect, it, vi } from 'vitest';
import type { FileContent, PatchSet, SearchHit } from '../domain/types';
import { TestBridge } from '../test/bridge';
import { Workbench } from './workbench';
const original: FileContent = { path: 'src/app.ts', content: 'old', hash: 'original-hash', encoding: 'utf-8' };
const patch: PatchSet = { id: 'patch-1', repositoryId: 'repo-1', status: 'proposed', source: 'agent:session', createdAt: 1790726400000, changes: [{ path: 'a.ts', before: 'a', after: 'b', originalHash: 'hash-a', resultHash: null, diff: '-a\n+b' }, { path: 'b.ts', before: null, after: 'new', originalHash: null, resultHash: null, diff: '+new' }] };
function deferred<T>() { let resolve: (value: T) => void = () => { throw new Error('Deferred promise not initialized'); }; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
describe('editor consistency', () => {
  it('saves with the original content hash and retains edits made during an in-flight save', async () => {
    const save = deferred<FileContent>();
    const bridge = new TestBridge({ read_file: () => original, write_file: () => save.promise, git_status: () => ({ branch: 'main', entries: [] }), git_branches: () => ['main'] });
    const workbench = new Workbench(bridge);
    await workbench.openFile(original.path); workbench.editFile(original.path, 'first draft');
    const pending = workbench.saveFile(); workbench.editFile(original.path, 'second draft');
    save.resolve({ ...original, content: 'first draft', hash: 'saved-hash' }); await pending;
    expect(bridge.calls.find(call => call.method === 'write_file')?.params).toEqual({ path: original.path, content: 'first draft', expectedHash: 'original-hash' });
    expect(workbench.editor.get().documents[0]).toMatchObject({ content: 'first draft', draft: 'second draft', hash: 'saved-hash' });
  });
  it('preserves a draft when storage rejects a stale hash', async () => {
    const bridge = new TestBridge({ read_file: () => original, write_file: () => { throw { code: 'CONFLICT', message: 'File changed on disk' }; } });
    const workbench = new Workbench(bridge); await workbench.openFile(original.path); workbench.editFile(original.path, 'draft');
    await workbench.perform('save', () => workbench.saveFile());
    expect(workbench.editor.get().documents[0]?.draft).toBe('draft'); expect(workbench.ui.get().notices[0]?.error.code).toBe('CONFLICT'); expect(workbench.ui.get().busy).toEqual([]);
  });
  it('refuses repository switching and closing when unsaved work exists', async () => {
    const workbench = new Workbench(new TestBridge({ read_file: () => original })); await workbench.openFile(original.path); workbench.editFile(original.path, 'draft');
    expect(workbench.closeFile(original.path)).toBe(false); await expect(workbench.openRepository('C:\\other')).rejects.toThrow('Save or discard'); expect(workbench.closeFile(original.path, true)).toBe(true);
  });
});
describe('indexed search concurrency', () => {
  it('does not let a slow earlier result overwrite the latest query', async () => {
    const first = deferred<SearchHit[]>(); const second = deferred<SearchHit[]>();
    const bridge = new TestBridge({ search: params => params.query === 'old' ? first.promise : second.promise }); const workbench = new Workbench(bridge);
    const older = workbench.runSearch('old', 'text'); const newer = workbench.runSearch('new', 'symbol');
    second.resolve([{ path: 'new.ts', line: 1, column: 1, content: 'new', symbol: 'new' }]); await newer; first.resolve([{ path: 'old.ts', line: 1, column: 1, content: 'old', symbol: null }]); await older;
    expect(workbench.search.get().hits[0]?.path).toBe('new.ts'); expect(workbench.search.get().query).toBe('new');
  });
  it('invalidates cancelled results without displaying them', async () => { const result = deferred<SearchHit[]>(); const workbench = new Workbench(new TestBridge({ search: () => result.promise })); const request = workbench.runSearch('a', 'text'); workbench.cancelSearch(); result.resolve([{ path: 'a', line: 1, column: 1, content: 'a', symbol: null }]); await request; expect(workbench.search.get().hits).toEqual([]); expect(workbench.search.get().loading).toBe(false); });
});
describe('command authorization and output bounds', () => {
  it('requires an explicit start after command classification', async () => {
    const bridge = new TestBridge({ classify_command: () => ({ level: 'CAUTION', reason: 'Runs repository scripts' }), start_command: () => 'process-1' }); const workbench = new Workbench(bridge);
    await workbench.prepareCommand({ program: 'npm', args: ['test'], cwd: null, env: {}, approved: true, isTest: true });
    expect(bridge.calls.map(call => call.method)).toEqual(['classify_command']); expect(workbench.terminal.get().pending?.spec.approved).toBe(false);
    await workbench.startCommand(true); expect(workbench.terminal.get().sessions[0]?.running).toBe(true); expect(bridge.calls[1]?.params).toMatchObject({ spec: { approved: true } });
  });
  it('never dispatches a blocked command', async () => { const bridge = new TestBridge({ classify_command: () => ({ level: 'BLOCKED', reason: 'System mutation is prohibited' }) }); const workbench = new Workbench(bridge); await workbench.prepareCommand({ program: 'danger', args: [], cwd: null, env: {}, approved: false, isTest: false }); await expect(workbench.startCommand(true)).rejects.toThrow('prohibited'); expect(bridge.calls).toHaveLength(1); });
  it('caps huge output and advances the streaming cursor', async () => {
    const bridge = new TestBridge({ classify_command: () => ({ level: 'SAFE', reason: 'Read-only' }), start_command: () => 'process-1', poll_command: params => ({ id: 'process-1', chunks: params.cursor === 0 ? [{ sequence: 1, stream: 'stdout', text: 'a'.repeat(700000) }] : [], nextCursor: 2, exitCode: 0, running: false, truncated: false }) });
    const workbench = new Workbench(bridge); await workbench.prepareCommand({ program: 'git', args: ['status'], cwd: null, env: {}, approved: false, isTest: false }); await workbench.startCommand(true); await workbench.poll();
    const session = workbench.terminal.get().sessions[0]; expect(session?.chunks[0]?.text.length).toBe(500000); expect(session?.truncated).toBe(true); expect(session?.cursor).toBe(2); expect(session?.exitCode).toBe(0); await workbench.poll(); expect(workbench.terminal.get().sessions[0]?.drained).toBe(true); await workbench.poll(); expect(bridge.calls.filter(call => call.method === 'poll_command')).toHaveLength(2);
  });
});
describe('patch approval invariants', () => {
  it('preserves hashes when selecting individual changes and retires the original proposal', async () => {
    const bridge = new TestBridge({ propose_patch: params => ({ ...patch, id: 'subset', changes: patch.changes.filter(change => params.proposals.some(proposal => proposal.path === change.path)) }), apply_patch: () => ({ ...patch, status: 'applied' }), reject_patch: () => ({ ...patch, status: 'rejected' }), agent_sessions: () => [], patches: () => [], git_status: () => ({ branch: 'main', entries: [] }), git_branches: () => ['main'] });
    const workbench = new Workbench(bridge); await workbench.applyPatch(patch, ['a.ts']);
    expect(bridge.calls[0]?.params).toMatchObject({ proposals: [{ path: 'a.ts', content: 'b', expectedHash: 'hash-a' }] }); expect(bridge.calls[1]).toEqual({ method: 'apply_patch', params: { id: 'subset' } }); expect(bridge.calls[2]).toEqual({ method: 'reject_patch', params: { id: 'patch-1' } });
  });
  it('rejects applying a patch over a dirty editor buffer before any backend write', async () => { const bridge = new TestBridge(); const workbench = new Workbench(bridge); workbench.editor.set({ active: 'a.ts', documents: [{ path: 'a.ts', content: 'a', draft: 'local', hash: 'hash-a', conflict: false, encoding: 'utf-8' }] }); await expect(workbench.applyPatch(patch, ['a.ts'])).rejects.toThrow('Save or discard'); expect(bridge.calls).toEqual([]); });
});
describe('filesystem invalidation', () => {
  it('reconciles a rescan without paths and preserves unchanged dirty buffers', async () => {
    const bridge = new TestBridge({ repositories: () => [], settings: () => new Workbench(new TestBridge()).settings.get(), tree: ({ path }) => [{ path: path ? `${path}/new.ts` : 'src', name: path ? 'new.ts' : 'src', kind: path ? 'file' : 'directory', size: 3 }], read_file: ({ path }) => ({ path, content: path === 'src/clean.ts' ? 'disk update' : 'base', hash: path === 'src/clean.ts' ? 'updated' : 'base-hash', encoding: 'utf-8' }), git_status: () => ({ branch: 'main', entries: [] }), git_branches: () => ['main'], health: () => ({ fileCount: 2 }) });
    const workbench = new Workbench(bridge); await workbench.initialize();
    workbench.repository.patch({ current: { id: 'repo', root: 'C:\\repo', name: 'repo' }, expanded: ['', 'src'], folders: { '': [], src: [], closed: [] } });
    workbench.editor.set({ active: 'src/clean.ts', documents: [{ path: 'src/clean.ts', content: 'base', draft: 'base', hash: 'base-hash', encoding: 'utf-8', conflict: false }, { path: 'src/dirty.ts', content: 'base', draft: 'my edits', hash: 'base-hash', encoding: 'utf-8', conflict: false }] });
    bridge.calls.length = 0; bridge.events.get('workspace_changed')?.({ paths: [], rescan: true });
    await vi.waitFor(() => expect(workbench.editor.get().documents[0]?.content).toBe('disk update'));
    expect(bridge.calls.filter(call => call.method === 'tree').map(call => call.params)).toEqual([{ path: '' }, { path: 'src' }]);
    expect(workbench.editor.get().documents[1]).toMatchObject({ draft: 'my edits', hash: 'base-hash', conflict: false });
    expect(workbench.repository.get().folders.src?.[0]?.name).toBe('new.ts'); expect(workbench.repository.get().health).toEqual({ fileCount: 2 }); workbench.dispose();
  });
  it('preserves edits made during rescan and flags a conflicting disk hash', async () => {
    const disk = deferred<FileContent>();
    const bridge = new TestBridge({ repositories: () => [], settings: () => new Workbench(new TestBridge()).settings.get(), tree: () => [], read_file: () => disk.promise, git_status: () => ({ branch: 'main', entries: [] }), git_branches: () => ['main'], health: () => null });
    const workbench = new Workbench(bridge); await workbench.initialize(); workbench.repository.patch({ current: { id: 'repo', root: 'C:\\repo', name: 'repo' }, expanded: [''] }); workbench.editor.set({ active: original.path, documents: [{ ...original, draft: original.content, conflict: false }] });
    bridge.events.get('workspace_changed')?.({ rescan: true }); workbench.editFile(original.path, 'draft made during refresh'); disk.resolve({ ...original, content: 'external', hash: 'external-hash' });
    await vi.waitFor(() => expect(workbench.editor.get().documents[0]?.conflict).toBe(true));
    expect(workbench.editor.get().documents[0]).toMatchObject({ draft: 'draft made during refresh', content: original.content, hash: original.hash }); workbench.dispose();
  });
});
