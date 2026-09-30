import { describe, expect, it, vi } from 'vitest';
import type { AgentSession, FileContent, Repository } from '../domain/types';
import { TestBridge } from '../test/bridge';
import { Workbench } from './workbench';
const file: FileContent = { path: 'a.ts', content: 'zero', hash: 'h0', encoding: 'utf-8' };
const repository: Repository = { id: 'a', root: 'C:/a', name: 'a' };
function deferred<T>() { let resolve: (value: T) => void = () => { throw new Error('Not initialized'); }; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
function readyBridge() { return new TestBridge({ repositories: () => [], settings: () => new Workbench(new TestBridge()).settings.get(), tree: () => [], git_status: () => ({ branch: 'main', entries: [] }), git_branches: () => [], health: () => null, agent_sessions: () => [], patches: () => [], open_repository: ({ path }) => ({ id: path, root: path, name: path }) }); }
describe('adversarial frontend scheduling', () => {
  it('does not replace an edited buffer when duplicate file opens complete', async () => {
    const first = deferred<FileContent>(); const second = deferred<FileContent>(); const bridge = readyBridge(); let reads = 0; bridge.handlers.read_file = () => ++reads === 1 ? first.promise : second.promise;
    const workbench = new Workbench(bridge); const one = workbench.openFile(file.path); const two = workbench.openFile(file.path); first.resolve(file); await one; workbench.editFile(file.path, 'unsaved user edit'); second.resolve(file); await two;
    expect(workbench.editor.get().documents[0]?.draft).toBe('unsaved user edit'); expect(reads).toBe(1);
  });
  it('converges to the newest invalidation after overlapping disk reads', async () => {
    const first = deferred<FileContent>(); const second = deferred<FileContent>(); const bridge = readyBridge(); let reads = 0; bridge.handlers.read_file = () => ++reads === 1 ? first.promise : second.promise;
    const workbench = new Workbench(bridge); await workbench.initialize(); workbench.repository.patch({ current: repository }); workbench.editor.set({ active: file.path, documents: [{ ...file, draft: file.content, conflict: false }] });
    bridge.events.get('workspace_changed')?.({ version: 1, repositoryId: 'a', sequence: 1, payload: { paths: [file.path], rescan: false } }); bridge.events.get('workspace_changed')?.({ version: 1, repositoryId: 'a', sequence: 2, payload: { paths: [file.path], rescan: false } }); first.resolve({ ...file, content: 'one', hash: 'h1' }); await vi.waitFor(() => expect(workbench.editor.get().documents[0]?.hash).toBe('h1')); second.resolve({ ...file, content: 'two', hash: 'h2' }); await vi.waitFor(() => expect(workbench.editor.get().documents[0]?.hash).toBe('h2')); workbench.dispose();
  });
  it('does not populate a new repository with a previous repository diff', async () => {
    const old = deferred<string>(); const bridge = readyBridge(); bridge.handlers.git_diff = () => old.promise; const workbench = new Workbench(bridge); workbench.repository.patch({ current: repository });
    const pending = workbench.perform('old diff', () => workbench.loadDiff('a.ts')); await workbench.openRepository('b'); old.resolve('private content from a'); await pending;
    expect(workbench.repository.get().current?.id).toBe('b'); expect(workbench.git.get().diff).toBe('');
  });
  it('coalesces an invalidation storm instead of starting an unbounded request queue', async () => {
    const disk = deferred<FileContent>(); const bridge = readyBridge(); bridge.handlers.read_file = () => disk.promise; const workbench = new Workbench(bridge); await workbench.initialize(); workbench.repository.patch({ current: repository }); workbench.editor.set({ active: file.path, documents: [{ ...file, draft: file.content, conflict: false }] }); bridge.calls.length = 0;
    const started = performance.now(); for (let index = 0; index < 1000; index++) bridge.events.get('workspace_changed')?.({ version: 1, repositoryId: 'a', sequence: index + 1, payload: { paths: [file.path], rescan: false } }); const reads = bridge.calls.filter(call => call.method === 'read_file').length;
    process.stdout.write(`${JSON.stringify({ profile: 'event-storm', events: 1000, immediateReads: reads, immediateRequests: bridge.calls.length, dispatchMs: performance.now() - started })}\n`); disk.resolve(file); await vi.waitFor(() => expect(workbench.editor.get().documents[0]?.hash).toBe('h0')); expect(reads).toBeLessThanOrEqual(1); workbench.dispose();
  });
  it('binds reads to their initiating repository and ignores superseded results within one repository', async () => {
    const older = deferred<string>(); const newer = deferred<string>(); const bridge = readyBridge(); bridge.handlers.git_diff = ({ path }) => path === 'old' ? older.promise : newer.promise; const workbench = new Workbench(bridge); workbench.repository.patch({ current: repository });
    const one = workbench.perform('old', () => workbench.loadDiff('old')); const two = workbench.loadDiff('new'); newer.resolve('new diff'); await two; older.resolve('old diff'); await one; expect(workbench.git.get().diff).toBe('new diff'); expect(bridge.bindings).toEqual(['a', 'a']); expect(workbench.ui.get().notices).toEqual([]);
  });
  it('blocks repository switches and duplicate execution while an approved command is starting', async () => {
    const start = deferred<string>(); const bridge = readyBridge(); bridge.handlers.classify_command = () => ({ level: 'CAUTION', reason: 'Test script' }); bridge.handlers.start_command = () => start.promise; const workbench = new Workbench(bridge); workbench.repository.patch({ current: repository }); await workbench.prepareCommand({ program: 'test', args: [], cwd: null, env: {}, approved: false, isTest: true });
    const pending = workbench.startCommand(true); await expect(workbench.startCommand(true)).rejects.toThrow('current operation'); await expect(workbench.openRepository('b')).rejects.toThrow('operations to finish'); expect(bridge.calls.filter(call => call.method === 'start_command')).toHaveLength(1); start.resolve('process-a'); await pending; expect(workbench.terminal.get().sessions[0]?.id).toBe('process-a');
  });
  it('ignores malformed, duplicate and other-repository events and reconciles a shared sequence gap', async () => {
    const bridge = readyBridge(); bridge.handlers.read_file = () => file; const workbench = new Workbench(bridge); await workbench.initialize(); workbench.repository.patch({ current: repository }); workbench.editor.set({ active: file.path, documents: [{ ...file, draft: file.content, conflict: false }] }); bridge.calls.length = 0;
    const emit = (sequence: number, repositoryId: string, paths: string[]) => bridge.events.get('workspace_changed')?.({ version: 1, repositoryId, sequence, payload: { paths, rescan: false } });
    emit(1, 'a', []); bridge.events.get('diagnostic')?.({ version: 1, repositoryId: null, sequence: 2, payload: { message: 'Global diagnostic' } }); emit(3, 'other', [file.path]); emit(2, 'a', [file.path]); bridge.events.get('workspace_changed')?.({ version: 2, repositoryId: 'a', sequence: 100, payload: { paths: [file.path], rescan: true } }); expect(bridge.calls).toHaveLength(0); emit(5, 'a', []);
    await vi.waitFor(() => expect(bridge.calls.filter(call => call.method === 'read_file')).toHaveLength(1)); expect(bridge.calls.filter(call => call.method === 'health')).toHaveLength(1); expect(workbench.ui.get().notices.some(notice => notice.error.code === 'EVENT_PROTOCOL')).toBe(true); workbench.dispose();
  });
  it('releases subscriptions that finish registering after the workbench was disposed', async () => {
    const gate = deferred<void>(); let stops = 0;
    class LateBridge extends TestBridge { override async subscribe(event: 'workspace_changed' | 'diagnostic', handler: (payload: unknown) => void): Promise<() => void> { const stop = await super.subscribe(event, handler); await gate.promise; return () => { stops++; stop(); }; } }
    const bridge = new LateBridge(); const workbench = new Workbench(bridge); const initialized = workbench.initialize(); workbench.dispose(); gate.resolve(); await initialized; expect(bridge.events.size).toBe(0); expect(stops).toBe(1); expect(bridge.calls).toHaveLength(0); expect(workbench.ui.get().ready).toBe(false);
  });
  it('does not discard a draft edited after a reload was confirmed', async () => { const disk = deferred<FileContent>(); const bridge = readyBridge(); bridge.handlers.read_file = () => disk.promise; const workbench = new Workbench(bridge); workbench.editor.set({ active: file.path, documents: [{ ...file, draft: 'previous draft', conflict: true }] }); const pending = workbench.reloadFile(file.path); workbench.editFile(file.path, 'newer draft'); disk.resolve({ ...file, hash: 'h1', content: 'disk' }); await expect(pending).rejects.toThrow('latest draft was preserved'); expect(workbench.editor.get().documents[0]?.draft).toBe('newer draft'); });
  it('does not revive an agent from an older poll after cancellation', async () => {
    const running: AgentSession = { id: 'agent', task: 'Task', status: 'running', createdAt: 0, nodes: [], edges: [], summary: '' }; const response = deferred<AgentSession>(); const bridge = readyBridge(); bridge.handlers.agent_session = () => response.promise; bridge.handlers.stop_agent = () => null; bridge.handlers.agent_sessions = () => [{ ...running, status: 'cancelled' }]; const workbench = new Workbench(bridge); workbench.agent.patch({ sessions: [running], active: running.id });
    const poll = workbench.poll(); await workbench.stopAgent(running.id); response.resolve(running); await poll; expect(workbench.agent.get().sessions[0]?.status).toBe('cancelled');
  });
  it('caps completed terminal sessions while keeping the newest session accessible', async () => {
    const bridge = readyBridge(); let counter = 0; bridge.handlers.classify_command = () => ({ level: 'SAFE', reason: 'Read' }); bridge.handlers.start_command = () => `command-${++counter}`; bridge.handlers.poll_command = ({ id, cursor }) => ({ id, chunks: [], nextCursor: cursor, exitCode: 0, running: false, truncated: false }); const workbench = new Workbench(bridge);
    for (let index = 0; index < 130; index++) { await workbench.prepareCommand({ program: 'git', args: ['status'], cwd: null, env: {}, approved: false, isTest: false }); await workbench.startCommand(true); await workbench.poll(); }
    expect(workbench.terminal.get().sessions).toHaveLength(128); expect(workbench.terminal.get().sessions[0]?.id).toBe('command-3'); expect(workbench.terminal.get().active).toBe('command-130');
  });
  it('surfaces patch recovery conflicts while preserving repository inspection', async () => {
    const bridge = readyBridge(); bridge.handlers.open_repository = () => ({ ...repository, recoveryRequired: { code: 'PATCH_RECOVERY_REQUIRED', message: 'External changes require recovery before mutation.', category: 'storage', recoverable: true, context: null } }); const workbench = new Workbench(bridge); await workbench.openRepository(repository.root); expect(workbench.repository.get().current?.id).toBe('a'); expect(workbench.ui.get().notices[0]?.error.code).toBe('PATCH_RECOVERY_REQUIRED');
  });
  it('preserves new editor changes made while a file deletion is in flight', async () => {
    const removed = deferred<null>(); const bridge = readyBridge(); bridge.handlers.read_file = () => file; bridge.handlers.remove_file = () => removed.promise; const workbench = new Workbench(bridge); workbench.editor.set({ active: file.path, documents: [{ ...file, draft: file.content, conflict: false }] }); const pending = workbench.removeFile(file.path); await vi.waitFor(() => expect(bridge.calls.some(call => call.method === 'remove_file')).toBe(true)); workbench.editFile(file.path, 'new draft'); removed.resolve(null); await pending; expect(workbench.editor.get().documents[0]).toMatchObject({ draft: 'new draft', conflict: true });
  });
});
