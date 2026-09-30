import type { Bridge } from '../services/bridge';
import { normalizeError } from '../services/bridge';
import { graphLocation } from '../services/navigation';
import { decodeEvent } from '../services/events';
import { appendOutput, processSessionLimit } from '../services/output';
import type { AgentSession, AppError, CommandSpec, EditorDocument, FileEntry, GitStatus, GraphEdge, IndexStats, Json, Methods, PatchSet, PolicyDecision, ProcessSession, Repository, SearchHit, SearchMode, Settings } from '../domain/types';
import { Store } from './store';
export interface RepositoryState { repositories: Repository[]; current: Repository | null; folders: Record<string, FileEntry[]>; expanded: string[]; selected: string; indexing: boolean; switching: boolean; index: IndexStats | null; health: Json }
export interface EditorState { documents: EditorDocument[]; active: string | null }
export interface SearchState { query: string; mode: SearchMode; hits: SearchHit[]; loading: boolean; more: boolean; offset: number; graph: GraphEdge[]; graphPath: string; direction: string }
export interface GitState { status: GitStatus | null; branches: string[]; diff: string; diffPath: string | null; staged: boolean; history: Json }
export interface AgentState { sessions: AgentSession[]; active: string | null; patches: PatchSet[] }
export interface TerminalState { sessions: ProcessSession[]; active: string | null; pending: { spec: CommandSpec; policy: PolicyDecision } | null }
export interface Notice { id: number; error: AppError }
export type Panel = 'explorer' | 'search' | 'git' | 'graph' | 'health';
export interface UiState { panel: Panel; explorer: boolean; terminal: boolean; agent: boolean; settings: boolean; palette: boolean; busy: string[]; notices: Notice[]; message: string; ready: boolean }
const fallbackSettings: Settings = { theme: 'system', provider: { endpoint: '', model: '', apiKeyEnv: '' }, maxAgentSteps: 12, contextBudget: 24000, testProgram: '', testArgs: [] };
class Superseded extends Error { constructor() { super('The workspace operation was superseded.'); } }
const globalMethods = new Set<string>(['repositories', 'open_repository', 'settings', 'save_settings', 'diagnostics', 'tool_schema']);
export class Workbench {
  readonly repository = new Store<RepositoryState>({ repositories: [], current: null, folders: {}, expanded: [], selected: '', indexing: false, switching: false, index: null, health: null });
  readonly editor = new Store<EditorState>({ documents: [], active: null });
  readonly search = new Store<SearchState>({ query: '', mode: 'text', hits: [], loading: false, more: false, offset: 0, graph: [], graphPath: '', direction: 'dependencies' });
  readonly git = new Store<GitState>({ status: null, branches: [], diff: '', diffPath: null, staged: false, history: null });
  readonly agent = new Store<AgentState>({ sessions: [], active: null, patches: [] });
  readonly terminal = new Store<TerminalState>({ sessions: [], active: null, pending: null });
  readonly settings = new Store<Settings>(fallbackSettings);
  readonly ui = new Store<UiState>({ panel: 'explorer', explorer: true, terminal: true, agent: true, settings: false, palette: false, busy: [], notices: [], message: 'Ready', ready: false });
  private noticeId = 0;
  private searchVersion = 0;
  private generation = 0;
  private stopSubscriptions: (() => void)[] = [];
  private polling = false;
  private disposed = false;
  private lifetime = 0;
  private initializing: Promise<void> | null = null;
  private initialized = false;
  private versions = new Map<string, number>();
  private mutations = new Map<string, Promise<unknown>>();
  private openingFiles = new Map<string, Promise<void>>();
  private fileIntent = 0;
  private eventSequence = 0;
  private invalidPaths = new Set<string>();
  private invalidRescan = false;
  private reconciling = false;
  private agentRevision = 0;
  private settingsRevision = 0;
  constructor(readonly bridge: Bridge) {}
  private async request<K extends keyof Methods>(method: K, params: Methods[K]['input']): Promise<Methods[K]['output']> {
    if (this.disposed) throw new Superseded();
    const global = globalMethods.has(method); const generation = this.generation; const lifetime = this.lifetime;
    if (!global && this.repository.get().switching) throw new Superseded();
    try {
      const result = await this.bridge.request(method, params, global ? null : this.repository.get().current?.id ?? null);
      if (this.disposed || lifetime !== this.lifetime || !global && generation !== this.generation) throw new Superseded();
      return result;
    } catch (error) { if (this.disposed || lifetime !== this.lifetime || !global && generation !== this.generation) throw new Superseded(); throw error; }
  }
  private async latest<K extends keyof Methods>(key: string, method: K, params: Methods[K]['input']): Promise<Methods[K]['output']> {
    const version = (this.versions.get(key) ?? 0) + 1; this.versions.set(key, version);
    const result = await this.request(method, params); if (this.versions.get(key) !== version) throw new Superseded(); return result;
  }
  private async mutate<T>(key: string, action: () => Promise<T>): Promise<T> {
    if (this.repository.get().switching || this.mutations.has(key)) throw new Error('Wait for the current operation to finish.');
    const task = Promise.resolve(); this.mutations.set(key, task);
    try { return await action(); } finally { if (this.mutations.get(key) === task) this.mutations.delete(key); }
  }
  report(error: unknown): void { if (error instanceof Superseded || this.disposed) return; const normalized = normalizeError(error); if (this.ui.get().notices.some(notice => notice.error.code === normalized.code && notice.error.message === normalized.message)) return; const notice = { id: ++this.noticeId, error: normalized }; this.ui.set(state => ({ ...state, notices: [...state.notices.slice(-19), notice], message: notice.error.message })); }
  dismiss(id: number): void { this.ui.set(state => ({ ...state, notices: state.notices.filter(notice => notice.id !== id) })); }
  async perform<T>(label: string, action: () => Promise<T>): Promise<T | undefined> {
    this.ui.set(state => ({ ...state, busy: [...state.busy, label] }));
    try { return await action(); } catch (error) { this.report(error); return undefined; }
    finally { this.ui.set(state => { const index = state.busy.indexOf(label); return { ...state, busy: state.busy.filter((_, position) => position !== index) }; }); }
  }
  async initialize(): Promise<void> {
    if (this.initializing) return this.initializing;
    if (this.initialized && !this.disposed) return;
    this.disposed = false; const lifetime = this.lifetime;
    if (!this.bridge.available) { this.ui.patch({ ready: true }); return; }
    const task = this.perform('Loading workspace', async () => {
      const subscribed: (() => void)[] = [];
      try {
        for (const event of ['workspace_changed', 'diagnostic'] as const) {
          const unlisten = await this.bridge.subscribe(event, payload => this.receiveEvent(event, payload)); let stopped = false; const stop = () => { if (!stopped) { stopped = true; unlisten(); } };
          if (this.disposed || lifetime !== this.lifetime) { stop(); throw new Superseded(); }
          subscribed.push(stop); this.stopSubscriptions.push(stop);
        }
        const settingsRevision = this.settingsRevision;
        const [repositories, settings] = await Promise.all([this.request('repositories', {}), this.request('settings', {})]);
        this.repository.patch({ repositories }); if (settingsRevision === this.settingsRevision) this.settings.set(settings); this.initialized = true;
      } catch (error) { subscribed.forEach(stop => stop()); this.stopSubscriptions = this.stopSubscriptions.filter(stop => !subscribed.includes(stop)); throw error; }
    }).then(() => { if (!this.disposed && lifetime === this.lifetime) this.ui.patch({ ready: true }); });
    this.initializing = task;
    try { await task; } finally { if (this.initializing === task) this.initializing = null; }
  }
  dispose(): void { this.disposed = true; this.initialized = false; this.lifetime++; this.stopSubscriptions.splice(0).forEach(stop => stop()); this.generation++; this.searchVersion++; this.invalidPaths.clear(); this.invalidRescan = false; this.initializing = null; }
  hasDirtyDocuments(): boolean { return this.editor.get().documents.some(document => document.content !== document.draft); }
  async openRepository(path: string, discard = false): Promise<void> {
    if (this.repository.get().switching || this.mutations.size) throw new Error('Wait for repository operations to finish before switching.');
    if (this.hasDirtyDocuments() && !discard) throw new Error('Save or discard edited documents before switching repositories.');
    const activeCommand = this.terminal.get().sessions.some(session => session.running);
    const activeAgent = this.agent.get().sessions.some(session => session.status === 'running');
    if (activeCommand || activeAgent) throw new Error('Stop active commands and agent tasks before switching repositories.');
    this.generation++;
    this.searchVersion++;
    this.repository.patch({ switching: true }); this.invalidPaths.clear(); this.invalidRescan = false; this.versions.clear(); this.openingFiles.clear(); this.fileIntent++;
    let current: Repository;
    try { current = await this.request('open_repository', { path }); }
    catch (error) { this.repository.patch({ switching: false }); this.queueInvalidation([], true); throw error; }
    this.repository.set({ repositories: [current, ...this.repository.get().repositories.filter(repository => repository.id !== current.id)].slice(0, 50), current, folders: {}, expanded: [''], selected: '', indexing: false, switching: false, index: null, health: null });
    this.editor.set({ documents: [], active: null });
    this.search.set({ query: '', mode: 'text', hits: [], loading: false, more: false, offset: 0, graph: [], graphPath: '', direction: 'dependencies' });
    this.git.set({ status: null, branches: [], diff: '', diffPath: null, staged: false, history: null });
    this.agent.set({ sessions: [], active: null, patches: [] });
    this.terminal.set({ sessions: [], active: null, pending: null });
    this.ui.patch({ message: `Opened ${current.name}`, panel: 'explorer' });
    if (current.recoveryRequired) this.report(current.recoveryRequired);
    const results = await Promise.allSettled([this.loadFolder(''), this.refreshGit(), this.refreshAgent(), this.refreshHealth()]);
    results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
  }
  async loadFolder(path: string): Promise<void> {
    const generation = this.generation;
    const entries = await this.latest(`folder:${path}`, 'tree', { path });
    if (generation !== this.generation) return;
    this.repository.set(state => ({ ...state, folders: { ...state.folders, [path]: entries } }));
  }
  async toggleFolder(path: string): Promise<void> {
    const state = this.repository.get();
    if (state.expanded.includes(path)) { this.repository.patch({ expanded: state.expanded.filter(entry => entry !== path) }); return; }
    this.repository.patch({ expanded: [...state.expanded, path] });
    if (!state.folders[path]) await this.loadFolder(path);
  }
  async openFile(path: string, line?: number): Promise<void> {
    const intent = ++this.fileIntent;
    const state = this.editor.get();
    this.repository.patch({ selected: path });
    if (state.documents.some(document => document.path === path)) { this.editor.set({ active: path, documents: state.documents.map(document => document.path === path ? { ...document, line } : document) }); return; }
    const pending = this.openingFiles.get(path);
    if (pending) { await pending; if (intent === this.fileIntent) this.editor.patch({ active: path }); return; }
    const task = (async () => {
      const file = await this.request('read_file', { path });
      this.editor.set(previous => ({ active: intent === this.fileIntent ? path : previous.active, documents: previous.documents.some(document => document.path === path) ? previous.documents : [...previous.documents, { ...file, draft: file.content, conflict: false, line }] }));
    })();
    this.openingFiles.set(path, task);
    try { await task; } finally { if (this.openingFiles.get(path) === task) this.openingFiles.delete(path); }
  }
  editFile(path: string, draft: string): void { if (this.repository.get().switching) return; this.editor.set(state => ({ ...state, documents: state.documents.map(document => document.path === path ? { ...document, draft } : document) })); }
  async saveFile(path = this.editor.get().active): Promise<void> { return this.mutate(`save:${path}`, async () => {
    const document = this.editor.get().documents.find(file => file.path === path);
    if (!document || document.draft === document.content) return;
    const draft = document.draft;
    const saved = await this.request('write_file', { path: document.path, content: draft, expectedHash: document.hash });
    this.editor.set(state => ({ ...state, documents: state.documents.map(file => file.path === saved.path ? file.hash !== document.hash && file.hash !== saved.hash ? { ...file, conflict: true } : { ...saved, draft: file.draft, conflict: false, line: file.line } : file) }));
    this.ui.patch({ message: `Saved ${saved.path}` });
    await this.refreshGit();
  }); }
  closeFile(path: string, discard = false): boolean {
    const state = this.editor.get();
    const document = state.documents.find(file => file.path === path);
    if (document && document.content !== document.draft && !discard) return false;
    const documents = state.documents.filter(file => file.path !== path);
    this.editor.set({ documents, active: state.active === path ? documents.at(-1)?.path ?? null : state.active });
    return true;
  }
  async reloadFile(path: string): Promise<void> { const original = this.editor.get().documents.find(document => document.path === path); const file = await this.latest(`reload:${path}`, 'read_file', { path }); const current = this.editor.get().documents.find(document => document.path === path); if (current !== original) throw new Error('The editor changed while reloading. Your latest draft was preserved.'); this.editor.set(state => ({ ...state, documents: state.documents.map(document => document.path === path ? { ...file, draft: file.content, conflict: false, line: document.line } : document) })); }
  async createFile(path: string, directory: boolean): Promise<void> { return this.mutate(`create:${path}`, async () => { await this.request('create_file', { path, directory }); await this.refreshFolders(); if (!directory) await this.openFile(path); }); }
  async renameFile(from: string, to: string): Promise<void> { return this.mutate(`rename:${from}`, async () => {
    if (this.editor.get().documents.some(document => (document.path === from || document.path.startsWith(`${from}/`)) && document.draft !== document.content)) throw new Error('Save edited files before renaming.');
    await this.request('rename_file', { from, to });
    this.editor.set(state => ({ active: state.active === from ? to : state.active?.startsWith(`${from}/`) ? `${to}${state.active.slice(from.length)}` : state.active, documents: state.documents.map(document => document.path === from || document.path.startsWith(`${from}/`) ? { ...document, path: `${to}${document.path.slice(from.length)}` } : document) }));
    this.repository.patch({ selected: to });
    await this.refreshFolders();
  }); }
  async removeFile(path: string): Promise<void> { return this.mutate(`remove:${path}`, async () => { const original = this.editor.get().documents.find(document => document.path === path); const file = await this.request('read_file', { path }); await this.request('remove_file', { path, expectedHash: file.hash }); const current = this.editor.get().documents.find(document => document.path === path); if (current && current !== original) { this.editor.set(state => ({ ...state, documents: state.documents.map(document => document.path === path ? { ...document, conflict: true } : document) })); this.report('The file was deleted, but editor changes made during deletion were preserved.'); } else this.closeFile(path, true); await this.refreshFolders(); }); }
  async refreshFolders(): Promise<void> { const results = await Promise.allSettled(this.repository.get().expanded.map(path => this.loadFolder(path))); results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); }); }
  private receiveEvent(name: 'workspace_changed' | 'diagnostic', value: unknown): void {
    if (this.disposed) return;
    const event = decodeEvent(name, value);
    if (!event) { this.report({ code: 'EVENT_PROTOCOL', message: 'A desktop event was rejected because its protocol or payload is invalid.' }); return; }
    if (event.sequence <= this.eventSequence) return;
    const gap = event.sequence !== this.eventSequence + 1;
    this.eventSequence = event.sequence;
    if (gap) this.queueInvalidation([], true);
    const current = this.repository.get();
    if (event.repositoryId !== null && event.repositoryId !== current.current?.id) return;
    if ('message' in event.payload) this.report(event.payload.message);
    else if (!current.switching) this.queueInvalidation(event.payload.paths, event.payload.rescan);
  }
  private queueInvalidation(paths: string[], rescan: boolean): void {
    if (this.disposed || !this.repository.get().current || this.repository.get().switching) return;
    this.invalidRescan ||= rescan;
    if (!this.invalidRescan) for (const path of paths) { this.invalidPaths.add(path); if (this.invalidPaths.size > 4096) { this.invalidRescan = true; break; } }
    if (this.invalidRescan) this.invalidPaths.clear();
    if (this.reconciling) return;
    this.reconciling = true;
    void this.drainInvalidations().catch(error => this.report(error)).finally(() => { this.reconciling = false; if (this.invalidRescan || this.invalidPaths.size) this.queueInvalidation([], false); });
  }
  private async drainInvalidations(): Promise<void> {
    while (!this.disposed && !this.repository.get().switching && (this.invalidRescan || this.invalidPaths.size)) {
      const paths = [...this.invalidPaths]; const rescan = this.invalidRescan; this.invalidPaths.clear(); this.invalidRescan = false;
      await this.changed(paths, rescan);
    }
  }
  private async changed(paths: string[], rescan: boolean): Promise<void> {
    if (!this.repository.get().current) return;
    const parents = new Set(paths.map(path => path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''));
    const affected = (path: string) => rescan || paths.some(changed => path === changed || path.startsWith(`${changed}/`));
    const generation = this.generation;
    const operations: (() => Promise<void>)[] = [...this.repository.get().expanded.filter(path => affected(path) || parents.has(path)).map(path => () => this.loadFolder(path)), () => this.refreshGit(), ...(rescan ? [() => this.refreshHealth()] : []), ...this.editor.get().documents.filter(document => affected(document.path)).map(document => async () => {
      try {
        const disk = await this.request('read_file', { path: document.path });
        if (generation !== this.generation) return;
        this.editor.set(state => ({ ...state, documents: state.documents.map(current => {
          if (current.path !== document.path || current.hash !== document.hash) return current;
          if (current.draft !== current.content) return { ...current, conflict: disk.hash !== current.hash };
          return { ...disk, draft: disk.content, conflict: false, line: current.line };
        }) }));
      } catch (error) {
        if (!(error instanceof Superseded) && generation === this.generation) this.editor.set(state => ({ ...state, documents: state.documents.map(current => current.path === document.path && current.hash === document.hash ? { ...current, conflict: true } : current) }));
        throw error;
      }
    })];
    for (let start = 0; start < operations.length && generation === this.generation; start += 4) {
      const results = await Promise.allSettled(operations.slice(start, start + 4).map(operation => operation()));
      results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
    }
  }
  async indexRepository(): Promise<void> { return this.mutate('index', async () => {
    this.repository.patch({ indexing: true });
    try { const index = await this.request('index_repository', {}); this.repository.patch({ index }); this.ui.patch({ message: index.cancelled ? 'Indexing cancelled' : `Indexed ${index.files.toLocaleString()} files in ${index.durationMs.toLocaleString()} ms` }); await this.refreshHealth(); }
    finally { this.repository.patch({ indexing: false }); }
  }); }
  async cancelIndex(): Promise<void> { await this.request('cancel_index', {}); }
  async runSearch(query: string, mode: SearchMode, append = false): Promise<void> {
    const version = ++this.searchVersion;
    if (!query.trim()) { this.search.patch({ query, mode, hits: [], loading: false, offset: 0, more: false }); return; }
    const offset = append ? this.search.get().hits.length : 0;
    this.search.patch({ query, mode, loading: true, offset });
    try { await this.request('cancel_search', {}); if (version !== this.searchVersion) return; const hits = await this.request('search', { query, mode, offset, limit: 100 }); if (version === this.searchVersion) this.search.set(state => ({ ...state, hits: append ? [...state.hits, ...hits] : hits, more: mode !== 'semantic' && hits.length === 100 })); }
    finally { if (version === this.searchVersion) this.search.patch({ loading: false }); }
  }
  async cancelSearch(): Promise<void> { this.searchVersion++; this.search.patch({ loading: false }); await this.request('cancel_search', {}); }
  async loadGraph(path: string, direction: string): Promise<void> { const graph = await this.latest('graph', 'graph', { path, direction }); this.search.patch({ graph, graphPath: path, direction }); }
  async openGraphLocation(edge: GraphEdge): Promise<void> { const location = graphLocation(edge, this.search.get().direction); if (location.external) throw new Error('External module references do not point to a local repository file.'); await this.openFile(location.path, location.line); }
  async refreshHealth(): Promise<void> { this.repository.patch({ health: await this.latest('health', 'health', {}) }); }
  async refreshGit(): Promise<void> { const generation = this.generation; const [status, branches] = await Promise.all([this.latest('git-status', 'git_status', {}), this.latest('git-branches', 'git_branches', {})]); if (generation === this.generation) this.git.patch({ status, branches }); }
  async loadDiff(path?: string, staged = false): Promise<void> { this.git.patch({ diff: await this.latest('diff', 'git_diff', { path, staged }), diffPath: path ?? null, staged }); }
  async loadHistory(path?: string): Promise<void> { this.git.patch({ history: await this.latest('history', 'git_history', { path }) }); }
  async gitAction(action: string, value: string): Promise<void> { return this.mutate('git-action', async () => { await this.request('git_action', { action, value }); await this.refreshGit(); await this.refreshFolders(); this.ui.patch({ message: `Git ${action} completed` }); }); }
  async prepareCommand(spec: CommandSpec): Promise<void> { const frozen = structuredClone({ ...spec, approved: false }); const policy = await this.latest('command-policy', 'classify_command', { spec: frozen }); this.terminal.patch({ pending: { spec: frozen, policy } }); this.ui.patch({ terminal: true }); }
  async startCommand(approved: boolean): Promise<void> { return this.mutate('start-command', async () => {
    const pending = this.terminal.get().pending;
    if (!pending) return;
    if (this.terminal.get().sessions.length >= processSessionLimit && !this.terminal.get().sessions.some(session => session.drained)) throw new Error('Close a finished process session before starting another command.');
    if (pending.policy.level.toUpperCase() === 'BLOCKED') throw new Error(pending.policy.reason);
    const spec = { ...pending.spec, approved };
    const id = await this.request('start_command', { spec });
    const session: ProcessSession = { id, label: `${spec.program} ${spec.args.join(' ')}`, spec, chunks: [], cursor: 0, running: true, drained: false, exitCode: null, truncated: false, startedAt: Date.now() };
    this.terminal.set(state => {
      const sessions = [...state.sessions, session];
      while (sessions.length > processSessionLimit) { const oldest = sessions.findIndex(entry => entry.drained); if (oldest < 0) break; sessions.splice(oldest, 1); }
      return { sessions, active: id, pending: state.pending === pending ? null : state.pending };
    });
  }); }
  async runTests(): Promise<void> { const settings = this.settings.get(); if (!settings.testProgram) { this.ui.patch({ settings: true }); throw new Error('Configure a test executable in Settings before running tests.'); } await this.prepareCommand({ program: settings.testProgram, args: settings.testArgs, cwd: null, env: {}, approved: false, isTest: true }); }
  async stopCommand(id: string): Promise<void> { await this.request('stop_command', { id }); }
  async sendInput(id: string, text: string): Promise<void> { await this.request('command_input', { id, text: `${text}\n` }); }
  closeCommand(id: string): void { const state = this.terminal.get(); if (state.sessions.find(session => session.id === id)?.running) return; const sessions = state.sessions.filter(session => session.id !== id); this.terminal.patch({ sessions, active: state.active === id ? sessions.at(-1)?.id ?? null : state.active }); }
  async refreshAgent(): Promise<void> { const revision = this.agentRevision; const [sessions, patches] = await Promise.all([this.latest('agent-list', 'agent_sessions', {}), this.latest('patches', 'patches', {})]); if (revision !== this.agentRevision) throw new Superseded(); this.agent.set(state => ({ sessions, patches, active: sessions.some(session => session.id === state.active) ? state.active : sessions[0]?.id ?? null })); }
  async startAgent(task: string, mode: 'task' | 'review'): Promise<void> { return this.mutate('start-agent', async () => { if (!task.trim()) throw new Error('Describe a task for the agent.'); this.agentRevision++; const session = await this.request('start_agent', { task, mode }); this.agent.set(state => ({ ...state, active: session.id, sessions: [session, ...state.sessions.filter(entry => entry.id !== session.id)].slice(0, 50) })); }); }
  async stopAgent(id: string): Promise<void> { return this.mutate(`agent:${id}`, async () => { this.agentRevision++; await this.request('stop_agent', { id }); await this.refreshAgent(); }); }
  async continueAgent(id: string): Promise<void> { return this.mutate(`agent:${id}`, async () => { this.agentRevision++; const session = await this.request('continue_agent', { id }); this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === id ? session : entry) })); }); }
  async approveAgentCommand(id: string): Promise<void> { return this.mutate(`agent:${id}`, async () => { this.agentRevision++; const session = await this.request('approve_agent_command', { id }); this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === id ? session : entry) })); }); }
  async applyPatch(patch: PatchSet, paths: string[]): Promise<void> { return this.mutate(`patch:${patch.id}`, async () => {
    const selected = patch.changes.filter(change => paths.includes(change.path));
    if (!selected.length) throw new Error('Select at least one change to apply.');
    if (this.editor.get().documents.some(document => paths.includes(document.path) && document.draft !== document.content)) throw new Error('Save or discard local editor changes before applying this patch.');
    const target = selected.length === patch.changes.length ? patch : await this.request('propose_patch', { proposals: selected.map(change => ({ path: change.path, content: change.after, expectedHash: change.originalHash })), source: `${patch.source}:selection:${patch.id}` });
    await this.request('apply_patch', { id: target.id });
    if (target.id !== patch.id) await this.request('reject_patch', { id: patch.id });
    await this.refreshAgent(); await this.refreshGit(); await this.refreshFolders();
    this.ui.patch({ message: 'Patch applied. Run relevant tests before continuing the agent.' });
  }); }
  async revertPatch(id: string): Promise<void> { return this.mutate(`patch:${id}`, async () => { await this.request('revert_patch', { id }); await this.refreshAgent(); await this.refreshGit(); await this.refreshFolders(); }); }
  async rejectPatch(id: string): Promise<void> { return this.mutate(`patch:${id}`, async () => { await this.request('reject_patch', { id }); await this.refreshAgent(); }); }
  async exportSession(id: string): Promise<string> { return this.request('export_session', { id }); }
  async importSession(json: string): Promise<void> { return this.mutate('import-session', async () => { const session = await this.request('import_session', { json }); await this.refreshAgent(); this.agent.patch({ active: session.id }); }); }
  async saveSettings(settings: Settings): Promise<void> { return this.mutate('settings', async () => { this.settingsRevision++; await this.request('save_settings', { settings }); this.settings.set(settings); this.ui.patch({ settings: false, message: 'Settings saved' }); }); }
  async poll(): Promise<void> {
    if (this.polling || !this.bridge.available) return;
    this.polling = true;
    const generation = this.generation;
    try {
      const commands = this.terminal.get().sessions.filter(session => !session.drained).map(async session => {
        const result = await this.request('poll_command', { id: session.id, cursor: session.cursor });
        if (generation !== this.generation) return;
        this.terminal.set(state => ({ ...state, sessions: state.sessions.map(entry => {
          if (entry.id !== session.id) return entry;
          return appendOutput(entry, result);
        }) }));
      });
      const agents = this.agent.get().sessions.filter(session => session.status === 'running').map(async session => {
        const revision = this.agentRevision;
        const result = await this.request('agent_session', { id: session.id });
        if (generation !== this.generation || revision !== this.agentRevision || this.agent.get().sessions.find(entry => entry.id === session.id) !== session) return;
        this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === result.id ? result : entry) }));
        if (result.status !== 'running') { const patches = await this.latest('patches', 'patches', {}); if (revision === this.agentRevision) this.agent.patch({ patches }); }
      });
      const results = await Promise.allSettled([...commands, ...agents]);
      results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
    } finally { this.polling = false; }
  }
}
