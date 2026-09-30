import type { Bridge } from '../services/bridge';
import { normalizeError } from '../services/bridge';
import { graphLocation } from '../services/navigation';
import type { AgentSession, AppError, CommandSpec, EditorDocument, FileEntry, GitStatus, GraphEdge, IndexStats, Json, PatchSet, PolicyDecision, ProcessSession, Repository, SearchHit, SearchMode, Settings } from '../domain/types';
import { Store } from './store';
export interface RepositoryState { repositories: Repository[]; current: Repository | null; folders: Record<string, FileEntry[]>; expanded: string[]; selected: string; indexing: boolean; index: IndexStats | null; health: Json }
export interface EditorState { documents: EditorDocument[]; active: string | null }
export interface SearchState { query: string; mode: SearchMode; hits: SearchHit[]; loading: boolean; more: boolean; offset: number; graph: GraphEdge[]; graphPath: string; direction: string }
export interface GitState { status: GitStatus | null; branches: string[]; diff: string; diffPath: string | null; staged: boolean; history: Json }
export interface AgentState { sessions: AgentSession[]; active: string | null; patches: PatchSet[] }
export interface TerminalState { sessions: ProcessSession[]; active: string | null; pending: { spec: CommandSpec; policy: PolicyDecision } | null }
export interface Notice { id: number; error: AppError }
export type Panel = 'explorer' | 'search' | 'git' | 'graph' | 'health';
export interface UiState { panel: Panel; explorer: boolean; terminal: boolean; agent: boolean; settings: boolean; palette: boolean; busy: string[]; notices: Notice[]; message: string; ready: boolean }
const fallbackSettings: Settings = { theme: 'system', provider: { endpoint: '', model: '', apiKeyEnv: '' }, maxAgentSteps: 12, contextBudget: 24000, testProgram: '', testArgs: [] };
export class Workbench {
  readonly repository = new Store<RepositoryState>({ repositories: [], current: null, folders: {}, expanded: [], selected: '', indexing: false, index: null, health: null });
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
  constructor(readonly bridge: Bridge) {}
  report(error: unknown): void { const normalized = normalizeError(error); if (this.ui.get().notices.some(notice => notice.error.code === normalized.code && notice.error.message === normalized.message)) return; const notice = { id: ++this.noticeId, error: normalized }; this.ui.set(state => ({ ...state, notices: [...state.notices.slice(-19), notice], message: notice.error.message })); }
  dismiss(id: number): void { this.ui.set(state => ({ ...state, notices: state.notices.filter(notice => notice.id !== id) })); }
  async perform<T>(label: string, action: () => Promise<T>): Promise<T | undefined> {
    this.ui.set(state => ({ ...state, busy: [...state.busy, label] }));
    try { return await action(); } catch (error) { this.report(error); return undefined; }
    finally { this.ui.set(state => { const index = state.busy.indexOf(label); return { ...state, busy: state.busy.filter((_, position) => position !== index) }; }); }
  }
  async initialize(): Promise<void> {
    if (!this.bridge.available) { this.ui.patch({ ready: true }); return; }
    await this.perform('Loading workspace', async () => {
      const [repositories, settings] = await Promise.all([this.bridge.request('repositories', {}), this.bridge.request('settings', {})]);
      this.repository.patch({ repositories });
      this.settings.set(settings);
      const stopWorkspace = await this.bridge.subscribe('workspace_changed', payload => { void this.changed(payload); });
      const stopDiagnostic = await this.bridge.subscribe('diagnostic', payload => { if (typeof payload === 'object' && payload !== null && 'message' in payload && typeof payload.message === 'string') this.report(payload.message); });
      this.stopSubscriptions.push(stopWorkspace, stopDiagnostic);
    });
    this.ui.patch({ ready: true });
  }
  dispose(): void { this.stopSubscriptions.splice(0).forEach(stop => stop()); this.generation++; this.searchVersion++; }
  hasDirtyDocuments(): boolean { return this.editor.get().documents.some(document => document.content !== document.draft); }
  async openRepository(path: string, discard = false): Promise<void> {
    if (this.hasDirtyDocuments() && !discard) throw new Error('Save or discard edited documents before switching repositories.');
    const activeCommand = this.terminal.get().sessions.some(session => session.running);
    const activeAgent = this.agent.get().sessions.some(session => session.status === 'running');
    if (activeCommand || activeAgent) throw new Error('Stop active commands and agent tasks before switching repositories.');
    const current = await this.bridge.request('open_repository', { path });
    this.generation++;
    this.searchVersion++;
    this.repository.set({ repositories: [current, ...this.repository.get().repositories.filter(repository => repository.id !== current.id)], current, folders: {}, expanded: [''], selected: '', indexing: false, index: null, health: null });
    this.editor.set({ documents: [], active: null });
    this.search.set({ query: '', mode: 'text', hits: [], loading: false, more: false, offset: 0, graph: [], graphPath: '', direction: 'dependencies' });
    this.git.set({ status: null, branches: [], diff: '', diffPath: null, staged: false, history: null });
    this.agent.set({ sessions: [], active: null, patches: [] });
    this.terminal.set({ sessions: [], active: null, pending: null });
    this.ui.patch({ message: `Opened ${current.name}`, panel: 'explorer' });
    const results = await Promise.allSettled([this.loadFolder(''), this.refreshGit(), this.refreshAgent(), this.refreshHealth()]);
    results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
  }
  async loadFolder(path: string): Promise<void> {
    const generation = this.generation;
    const entries = await this.bridge.request('tree', { path });
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
    const state = this.editor.get();
    this.repository.patch({ selected: path });
    if (state.documents.some(document => document.path === path)) { this.editor.set({ active: path, documents: state.documents.map(document => document.path === path ? { ...document, line } : document) }); return; }
    const generation = this.generation;
    const file = await this.bridge.request('read_file', { path });
    if (generation !== this.generation) return;
    this.editor.set(previous => ({ active: path, documents: [...previous.documents.filter(document => document.path !== path), { ...file, draft: file.content, conflict: false, line }] }));
  }
  editFile(path: string, draft: string): void { this.editor.set(state => ({ ...state, documents: state.documents.map(document => document.path === path ? { ...document, draft } : document) })); }
  async saveFile(path = this.editor.get().active): Promise<void> {
    const document = this.editor.get().documents.find(file => file.path === path);
    if (!document || document.draft === document.content) return;
    const draft = document.draft;
    const saved = await this.bridge.request('write_file', { path: document.path, content: draft, expectedHash: document.hash });
    this.editor.set(state => ({ ...state, documents: state.documents.map(file => file.path === saved.path ? { ...saved, draft: file.draft, conflict: false } : file) }));
    this.ui.patch({ message: `Saved ${saved.path}` });
    await this.refreshGit();
  }
  closeFile(path: string, discard = false): boolean {
    const state = this.editor.get();
    const document = state.documents.find(file => file.path === path);
    if (document && document.content !== document.draft && !discard) return false;
    const documents = state.documents.filter(file => file.path !== path);
    this.editor.set({ documents, active: state.active === path ? documents.at(-1)?.path ?? null : state.active });
    return true;
  }
  async reloadFile(path: string): Promise<void> { const file = await this.bridge.request('read_file', { path }); this.editor.set(state => ({ ...state, documents: state.documents.map(document => document.path === path ? { ...file, draft: file.content, conflict: false } : document) })); }
  async createFile(path: string, directory: boolean): Promise<void> { await this.bridge.request('create_file', { path, directory }); await this.refreshFolders(); if (!directory) await this.openFile(path); }
  async renameFile(from: string, to: string): Promise<void> {
    if (this.editor.get().documents.some(document => (document.path === from || document.path.startsWith(`${from}/`)) && document.draft !== document.content)) throw new Error('Save edited files before renaming.');
    await this.bridge.request('rename_file', { from, to });
    this.editor.set(state => ({ active: state.active === from ? to : state.active?.startsWith(`${from}/`) ? `${to}${state.active.slice(from.length)}` : state.active, documents: state.documents.map(document => document.path === from || document.path.startsWith(`${from}/`) ? { ...document, path: `${to}${document.path.slice(from.length)}` } : document) }));
    this.repository.patch({ selected: to });
    await this.refreshFolders();
  }
  async removeFile(path: string): Promise<void> { const file = await this.bridge.request('read_file', { path }); await this.bridge.request('remove_file', { path, expectedHash: file.hash }); this.closeFile(path, true); await this.refreshFolders(); }
  async refreshFolders(): Promise<void> { const results = await Promise.allSettled(this.repository.get().expanded.map(path => this.loadFolder(path))); results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); }); }
  private async changed(payload: unknown): Promise<void> {
    if (!this.repository.get().current || typeof payload !== 'object' || payload === null) return;
    const rescan = 'rescan' in payload && payload.rescan === true;
    if (!rescan && (!('paths' in payload) || !Array.isArray(payload.paths))) return;
    const paths = 'paths' in payload && Array.isArray(payload.paths) ? payload.paths.filter((path): path is string => typeof path === 'string') : [];
    const parents = new Set(paths.map(path => path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''));
    const affected = (path: string) => rescan || paths.some(changed => path === changed || path.startsWith(`${changed}/`));
    const generation = this.generation;
    const results = await Promise.allSettled([...this.repository.get().expanded.filter(path => affected(path) || parents.has(path)).map(path => this.loadFolder(path)), this.refreshGit(), ...(rescan ? [this.refreshHealth()] : []), ...this.editor.get().documents.filter(document => affected(document.path)).map(async document => {
      try {
        const disk = await this.bridge.request('read_file', { path: document.path });
        if (generation !== this.generation) return;
        this.editor.set(state => ({ ...state, documents: state.documents.map(current => {
          if (current.path !== document.path || current.hash !== document.hash) return current;
          if (current.draft !== current.content) return { ...current, conflict: disk.hash !== current.hash };
          return { ...disk, draft: disk.content, conflict: false, line: current.line };
        }) }));
      } catch (error) {
        if (generation === this.generation) this.editor.set(state => ({ ...state, documents: state.documents.map(current => current.path === document.path ? { ...current, conflict: true } : current) }));
        throw error;
      }
    })]);
    results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
  }
  async indexRepository(): Promise<void> {
    this.repository.patch({ indexing: true });
    try { const index = await this.bridge.request('index_repository', {}); this.repository.patch({ index }); this.ui.patch({ message: index.cancelled ? 'Indexing cancelled' : `Indexed ${index.files.toLocaleString()} files in ${index.durationMs.toLocaleString()} ms` }); await this.refreshHealth(); }
    finally { this.repository.patch({ indexing: false }); }
  }
  async cancelIndex(): Promise<void> { await this.bridge.request('cancel_index', {}); }
  async runSearch(query: string, mode: SearchMode, append = false): Promise<void> {
    const version = ++this.searchVersion;
    if (!query.trim()) { this.search.patch({ query, mode, hits: [], loading: false, offset: 0, more: false }); return; }
    const offset = append ? this.search.get().hits.length : 0;
    this.search.patch({ query, mode, loading: true, offset });
    try { await this.bridge.request('cancel_search', {}); if (version !== this.searchVersion) return; const hits = await this.bridge.request('search', { query, mode, offset, limit: 100 }); if (version === this.searchVersion) this.search.set(state => ({ ...state, hits: append ? [...state.hits, ...hits] : hits, more: mode !== 'semantic' && hits.length === 100 })); }
    finally { if (version === this.searchVersion) this.search.patch({ loading: false }); }
  }
  async cancelSearch(): Promise<void> { this.searchVersion++; this.search.patch({ loading: false }); await this.bridge.request('cancel_search', {}); }
  async loadGraph(path: string, direction: string): Promise<void> { const graph = await this.bridge.request('graph', { path, direction }); this.search.patch({ graph, graphPath: path, direction }); }
  async openGraphLocation(edge: GraphEdge): Promise<void> { const location = graphLocation(edge, this.search.get().direction); if (location.external) throw new Error('External module references do not point to a local repository file.'); await this.openFile(location.path, location.line); }
  async refreshHealth(): Promise<void> { this.repository.patch({ health: await this.bridge.request('health', {}) }); }
  async refreshGit(): Promise<void> { const generation = this.generation; const [status, branches] = await Promise.all([this.bridge.request('git_status', {}), this.bridge.request('git_branches', {})]); if (generation === this.generation) this.git.patch({ status, branches }); }
  async loadDiff(path?: string, staged = false): Promise<void> { this.git.patch({ diff: await this.bridge.request('git_diff', { path, staged }), diffPath: path ?? null, staged }); }
  async loadHistory(path?: string): Promise<void> { this.git.patch({ history: await this.bridge.request('git_history', { path }) }); }
  async gitAction(action: string, value: string): Promise<void> { await this.bridge.request('git_action', { action, value }); await this.refreshGit(); await this.refreshFolders(); this.ui.patch({ message: `Git ${action} completed` }); }
  async prepareCommand(spec: CommandSpec): Promise<void> { const policy = await this.bridge.request('classify_command', { spec: { ...spec, approved: false } }); this.terminal.patch({ pending: { spec: { ...spec, approved: false }, policy } }); this.ui.patch({ terminal: true }); }
  async startCommand(approved: boolean): Promise<void> {
    const pending = this.terminal.get().pending;
    if (!pending) return;
    if (pending.policy.level.toUpperCase() === 'BLOCKED') throw new Error(pending.policy.reason);
    const spec = { ...pending.spec, approved };
    const id = await this.bridge.request('start_command', { spec });
    const session: ProcessSession = { id, label: `${spec.program} ${spec.args.join(' ')}`, spec, chunks: [], cursor: 0, running: true, drained: false, exitCode: null, truncated: false, startedAt: Date.now() };
    this.terminal.set(state => ({ sessions: [...state.sessions, session], active: id, pending: null }));
  }
  async runTests(): Promise<void> { const settings = this.settings.get(); if (!settings.testProgram) { this.ui.patch({ settings: true }); throw new Error('Configure a test executable in Settings before running tests.'); } await this.prepareCommand({ program: settings.testProgram, args: settings.testArgs, cwd: null, env: {}, approved: false, isTest: true }); }
  async stopCommand(id: string): Promise<void> { await this.bridge.request('stop_command', { id }); }
  async sendInput(id: string, text: string): Promise<void> { await this.bridge.request('command_input', { id, text: `${text}\n` }); }
  closeCommand(id: string): void { const state = this.terminal.get(); if (state.sessions.find(session => session.id === id)?.running) return; const sessions = state.sessions.filter(session => session.id !== id); this.terminal.patch({ sessions, active: state.active === id ? sessions.at(-1)?.id ?? null : state.active }); }
  async refreshAgent(): Promise<void> { const [sessions, patches] = await Promise.all([this.bridge.request('agent_sessions', {}), this.bridge.request('patches', {})]); this.agent.set(state => ({ sessions, patches, active: sessions.some(session => session.id === state.active) ? state.active : sessions[0]?.id ?? null })); }
  async startAgent(task: string, mode: 'task' | 'review'): Promise<void> { if (!task.trim()) throw new Error('Describe a task for the agent.'); const session = await this.bridge.request('start_agent', { task, mode }); this.agent.set(state => ({ ...state, active: session.id, sessions: [session, ...state.sessions.filter(entry => entry.id !== session.id)] })); }
  async stopAgent(id: string): Promise<void> { await this.bridge.request('stop_agent', { id }); await this.refreshAgent(); }
  async continueAgent(id: string): Promise<void> { const session = await this.bridge.request('continue_agent', { id }); this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === id ? session : entry) })); }
  async approveAgentCommand(id: string): Promise<void> { const session = await this.bridge.request('approve_agent_command', { id }); this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === id ? session : entry) })); }
  async applyPatch(patch: PatchSet, paths: string[]): Promise<void> {
    const selected = patch.changes.filter(change => paths.includes(change.path));
    if (!selected.length) throw new Error('Select at least one change to apply.');
    if (this.editor.get().documents.some(document => paths.includes(document.path) && document.draft !== document.content)) throw new Error('Save or discard local editor changes before applying this patch.');
    const target = selected.length === patch.changes.length ? patch : await this.bridge.request('propose_patch', { proposals: selected.map(change => ({ path: change.path, content: change.after, expectedHash: change.originalHash })), source: `${patch.source}:selection:${patch.id}` });
    await this.bridge.request('apply_patch', { id: target.id });
    if (target.id !== patch.id) await this.bridge.request('reject_patch', { id: patch.id });
    await this.refreshAgent(); await this.refreshGit(); await this.refreshFolders();
    this.ui.patch({ message: 'Patch applied. Run relevant tests before continuing the agent.' });
  }
  async revertPatch(id: string): Promise<void> { await this.bridge.request('revert_patch', { id }); await this.refreshAgent(); await this.refreshGit(); await this.refreshFolders(); }
  async rejectPatch(id: string): Promise<void> { await this.bridge.request('reject_patch', { id }); await this.refreshAgent(); }
  async exportSession(id: string): Promise<string> { return this.bridge.request('export_session', { id }); }
  async importSession(json: string): Promise<void> { const session = await this.bridge.request('import_session', { json }); await this.refreshAgent(); this.agent.patch({ active: session.id }); }
  async saveSettings(settings: Settings): Promise<void> { await this.bridge.request('save_settings', { settings }); this.settings.set(settings); this.ui.patch({ settings: false, message: 'Settings saved' }); }
  async poll(): Promise<void> {
    if (this.polling || !this.bridge.available) return;
    this.polling = true;
    const generation = this.generation;
    try {
      const commands = this.terminal.get().sessions.filter(session => !session.drained).map(async session => {
        const result = await this.bridge.request('poll_command', { id: session.id, cursor: session.cursor });
        if (generation !== this.generation) return;
        this.terminal.set(state => ({ ...state, sessions: state.sessions.map(entry => {
          if (entry.id !== session.id) return entry;
          let chunks = [...entry.chunks, ...result.chunks];
          let length = chunks.reduce((sum, chunk) => sum + chunk.text.length, 0);
          let truncated = entry.truncated || result.truncated;
          while (length > 500000 && chunks.length > 1) { length -= chunks[0]?.text.length ?? 0; chunks = chunks.slice(1); truncated = true; }
          if (chunks[0] && chunks[0].text.length > 500000) { chunks[0] = { ...chunks[0], text: chunks[0].text.slice(-500000) }; truncated = true; }
          return { ...entry, chunks, cursor: result.nextCursor, running: result.running, drained: !result.running && result.chunks.length === 0, exitCode: result.exitCode, truncated };
        }) }));
      });
      const agents = this.agent.get().sessions.filter(session => session.status === 'running').map(async session => {
        const result = await this.bridge.request('agent_session', { id: session.id });
        if (generation !== this.generation) return;
        this.agent.set(state => ({ ...state, sessions: state.sessions.map(entry => entry.id === result.id ? result : entry) }));
        if (result.status !== 'running') this.agent.patch({ patches: await this.bridge.request('patches', {}) });
      });
      const results = await Promise.allSettled([...commands, ...agents]);
      results.forEach(result => { if (result.status === 'rejected') this.report(result.reason); });
    } finally { this.polling = false; }
  }
}
