import { useCallback, useEffect, useState } from 'react';
import { Activity, ChevronDown, Code2, Command, Files, FolderOpen, GitBranch, Network, PanelBottom, PanelLeft, Search, Settings2, Sparkles, X, LoaderCircle, Terminal, ShieldCheck } from 'lucide-react';
import type { Workbench, Panel } from './state/workbench';
import { defaultShortcuts, loadShortcuts, matchesShortcut } from './services/shortcuts';
import type { ShortcutAction } from './services/shortcuts';
import { commandSpec } from './services/commands';
import { closeNativeWindow, watchNativeClose } from './services/native';
import { WorkbenchContext, useStore } from './ui/context';
import { Explorer } from './ui/Explorer';
import { SearchPanel, GraphPanel, HealthPanel } from './ui/SearchPanel';
import { GitPanel } from './ui/GitPanel';
import { EditorArea } from './ui/EditorArea';
import { TerminalPanel } from './ui/TerminalPanel';
import { AgentPanel } from './ui/AgentPanel';
import { CommandPalette } from './ui/CommandPalette';
import type { PaletteCommand } from './ui/CommandPalette';
import { SettingsDialog } from './ui/SettingsDialog';
import { ConfirmDialog, IconButton, InputDialog } from './ui/primitives';
export function App({ workbench }: { workbench: Workbench }) {
  const ui = useStore(workbench.ui);
  const repository = useStore(workbench.repository);
  const git = useStore(workbench.git);
  const editor = useStore(workbench.editor);
  const settings = useStore(workbench.settings);
  const agent = useStore(workbench.agent);
  const [openDialog, setOpenDialog] = useState(false);
  const [buildDialog, setBuildDialog] = useState(false);
  const [closeDialog, setCloseDialog] = useState(false);
  const [bindings, setBindings] = useState(defaultShortcuts);
  const [systemDark, setSystemDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches);
  const theme = settings.theme === 'system' ? systemDark ? 'dark' : 'light' : settings.theme;
  const showPanel = useCallback((panel: Panel) => { workbench.ui.patch({ panel, explorer: true }); }, [workbench]);
  const toggle = useCallback((key: 'terminal' | 'agent' | 'explorer') => { workbench.ui.set(state => ({ ...state, [key]: !state[key] })); }, [workbench]);
  const openRepository = useCallback(() => setOpenDialog(true), []);
  const closePalette = useCallback(() => workbench.ui.patch({ palette: false }), [workbench]);
  const closeSettings = useCallback(() => workbench.ui.patch({ settings: false }), [workbench]);
  useEffect(() => {
    void workbench.initialize();
    try { setBindings(loadShortcuts()); } catch (error) { workbench.report(error); }
    const timer = setInterval(() => { void workbench.poll(); }, 500);
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const changed = () => setSystemDark(media.matches);
    media.addEventListener('change', changed);
    const beforeUnload = (event: BeforeUnloadEvent) => { if (workbench.hasDirtyDocuments()) event.preventDefault(); };
    window.addEventListener('beforeunload', beforeUnload);
    return () => { clearInterval(timer); workbench.dispose(); media.removeEventListener('change', changed); window.removeEventListener('beforeunload', beforeUnload); };
  }, [workbench]);
  useEffect(() => {
    let disposed = false;
    let stop: () => void = () => undefined;
    void watchNativeClose(() => workbench.hasDirtyDocuments() || workbench.terminal.get().sessions.some(session => session.running) || workbench.agent.get().sessions.some(session => session.status === 'running'), () => setCloseDialog(true)).then(unlisten => { if (disposed) unlisten(); else stop = unlisten; }).catch((error: unknown) => workbench.report(error));
    return () => { disposed = true; stop(); };
  }, [workbench]);
  const shortcut = useCallback((action: ShortcutAction) => {
    if (action === 'palette') workbench.ui.patch({ palette: true });
    if (action === 'settings') workbench.ui.patch({ settings: true });
    if (action === 'files') { workbench.search.patch({ mode: 'filename' }); showPanel('search'); }
    if (action === 'search') { workbench.search.patch({ mode: 'text' }); showPanel('search'); }
    if (action === 'save') void workbench.perform('Saving file', () => workbench.saveFile());
    if (action === 'terminal' || action === 'agent' || action === 'explorer') toggle(action);
  }, [showPanel, toggle, workbench]);
  useEffect(() => { const keydown = (event: KeyboardEvent) => { for (const action of Object.keys(bindings) as ShortcutAction[]) { if (matchesShortcut(event, bindings[action])) { event.preventDefault(); shortcut(action); break; } } }; window.addEventListener('keydown', keydown); return () => window.removeEventListener('keydown', keydown); }, [bindings, shortcut]);
  const commands: PaletteCommand[] = [
    { id: 'open', label: 'Open Repository', detail: 'Select a local Git repository by path', run: openRepository, disabled: !workbench.bridge.available },
    { id: 'files', label: 'Open File', detail: 'Search indexed file names', shortcut: 'Ctrl P', run: () => shortcut('files'), disabled: !repository.current },
    { id: 'search', label: 'Search Repository', detail: 'Search text, regex, and semantic context', shortcut: 'Ctrl ⇧ F', run: () => shortcut('search'), disabled: !repository.current },
    { id: 'symbol', label: 'Search Symbol', detail: 'Navigate indexed definitions', run: () => { workbench.search.patch({ mode: 'symbol' }); showPanel('search'); }, disabled: !repository.current },
    { id: 'references', label: 'Find References', detail: 'Inspect indexed code relationships', run: () => { workbench.search.patch({ direction: 'references' }); showPanel('graph'); }, disabled: !repository.current },
    { id: 'tests', label: 'Run Tests', detail: 'Review and execute configured test command', run: () => { void workbench.perform('Preparing tests', () => workbench.runTests()); }, disabled: !repository.current },
    { id: 'build', label: 'Run Build', detail: 'Enter a build executable for policy review', run: () => setBuildDialog(true), disabled: !repository.current },
    { id: 'git', label: 'Git Status', detail: 'Inspect staged and working tree changes', run: () => showPanel('git'), disabled: !repository.current },
    { id: 'diff', label: 'Git Diff', detail: 'Show the working tree diff', run: () => { showPanel('git'); void workbench.perform('Loading diff', () => workbench.loadDiff()); }, disabled: !repository.current },
    { id: 'index', label: 'Index Repository', detail: 'Build persistent search and symbol indexes', run: () => { void workbench.perform('Indexing repository', () => workbench.indexRepository()); }, disabled: !repository.current || repository.indexing },
    { id: 'start-agent', label: 'Start AI Task', detail: 'Open task composer and execution trace', run: () => workbench.ui.patch({ agent: true }), disabled: !repository.current },
    { id: 'stop-agent', label: 'Stop AI Task', detail: 'Cancel the active agent execution', run: () => { if (agent.active) void workbench.perform('Stopping agent', () => workbench.stopAgent(agent.active ?? '')); }, disabled: !agent.sessions.some(session => session.id === agent.active && session.status === 'running') },
    { id: 'settings', label: 'Open Settings', detail: 'Provider, theme, tests, and shortcuts', run: () => shortcut('settings') },
    { id: 'terminal', label: 'Toggle Terminal', detail: 'Show or hide process output', shortcut: 'Ctrl `', run: () => toggle('terminal') },
    { id: 'agent', label: 'Toggle Agent', detail: 'Show or hide engineering agent', shortcut: 'Ctrl ⇧ A', run: () => toggle('agent') },
    { id: 'explorer', label: 'Toggle Explorer', detail: 'Show or hide the repository sidebar', shortcut: 'Ctrl B', run: () => toggle('explorer') },
  ];
  const nav = [{ panel: 'explorer' as const, label: 'Explorer', icon: Files }, { panel: 'search' as const, label: 'Search', icon: Search }, { panel: 'git' as const, label: 'Source control', icon: GitBranch }, { panel: 'graph' as const, label: 'Code relationships', icon: Network }, { panel: 'health' as const, label: 'Repository health', icon: Activity }];
  const activeDocument = editor.documents.find(document => document.path === editor.active);
  return <WorkbenchContext.Provider value={workbench}><div className={`app theme-${theme}`}><header className="titlebar"><div className="brand"><div className="brand-symbol"><Code2 size={19} strokeWidth={1.7} /></div><span>AstraForge</span><span className="version">WORKBENCH</span></div><button className="workspace-selector" onClick={openRepository} disabled={!workbench.bridge.available}><FolderOpen size={13} /><span>{repository.current?.name ?? 'Open a repository'}</span><ChevronDown size={13} /></button><button className="global-search" onClick={() => workbench.ui.patch({ palette: true })}><Search size={13} /><span>Search commands…</span><kbd>Ctrl ⇧ P</kbd></button><div className="layout-controls"><IconButton label="Toggle explorer" onClick={() => toggle('explorer')}><PanelLeft size={16} /></IconButton><IconButton label="Toggle terminal" onClick={() => toggle('terminal')}><PanelBottom size={16} /></IconButton><IconButton label="Toggle agent" onClick={() => toggle('agent')}><Sparkles size={16} /></IconButton></div></header><main className={`workbench ${ui.explorer ? '' : 'without-explorer'} ${ui.agent ? '' : 'without-agent'}`}><nav className="activity-bar" aria-label="Workspace views"><div>{nav.map(({ panel, label, icon: Icon }) => <button key={panel} title={label} aria-label={label} aria-current={ui.panel === panel && ui.explorer ? 'page' : undefined} className={ui.panel === panel && ui.explorer ? 'active' : ''} onClick={() => showPanel(panel)}><Icon size={21} strokeWidth={1.6} />{panel === 'git' && Boolean(git.status?.entries.length) && <span className="activity-badge">{git.status?.entries.length}</span>}</button>)}</div><div><button title="Command palette" aria-label="Command palette" onClick={() => workbench.ui.patch({ palette: true })}><Command size={20} strokeWidth={1.5} /></button><button title="Settings" aria-label="Settings" onClick={() => workbench.ui.patch({ settings: true })}><Settings2 size={20} strokeWidth={1.5} /></button></div></nav>{ui.explorer && <div className="sidebar">{ui.panel === 'explorer' && <Explorer />}{ui.panel === 'search' && <SearchPanel key={`${workbench.search.get().mode}`} />}{ui.panel === 'git' && <GitPanel />}{ui.panel === 'graph' && <GraphPanel />}{ui.panel === 'health' && <HealthPanel />}</div>}<div className="main-column"><EditorArea theme={theme} onOpenRepository={openRepository} />{ui.terminal && <TerminalPanel />}</div>{ui.agent && <AgentPanel />}</main><footer className="statusbar"><span className="remote-indicator"><Code2 size={13} /></span><button title="Source control" onClick={() => showPanel('git')}><GitBranch size={12} />{git.status?.branch ?? 'No repository'}</button><span className="status-divider" /><button title="Repository health" onClick={() => showPanel('health')}>{repository.indexing ? <LoaderCircle size={12} className="spin" /> : <Activity size={12} />}{repository.indexing ? 'Indexing' : repository.index ? `${repository.index.files} files indexed` : 'Index'}</button><span className="status-message">{ui.busy[0] ? <><LoaderCircle size={11} className="spin" />{ui.busy[0]}</> : ui.message}</span><span className="spacer" /><span className="encoding">{activeDocument?.encoding ?? 'UTF-8'}</span><span className="local-status"><ShieldCheck size={12} />Local workspace</span><button title="Toggle terminal" onClick={() => toggle('terminal')}><Terminal size={12} /></button><button title="Toggle agent" onClick={() => toggle('agent')}><Sparkles size={12} /></button></footer>{ui.notices.slice(-3).map((notice, index) => <div className="toast" role="alert" style={{ bottom: 38 + index * 84 }} key={notice.id}><div><strong>{notice.error.code.replaceAll('_', ' ')}</strong><span>{notice.error.message}</span></div><IconButton label="Dismiss error" onClick={() => workbench.dismiss(notice.id)}><X size={14} /></IconButton></div>)}{openDialog && <InputDialog title="Open repository" label="Absolute path to a local Git repository" initial={repository.current?.root ?? ''} submit="Open repository" onClose={() => setOpenDialog(false)} onConfirm={async path => { const result = await workbench.perform('Opening repository', async () => { await workbench.openRepository(path); return true; }); if (result) { setOpenDialog(false); void workbench.perform('Indexing repository', () => workbench.indexRepository()); } }} />}{buildDialog && <InputDialog title="Run build" label="Build command, executable followed by arguments" submit="Review command" onClose={() => setBuildDialog(false)} onConfirm={async value => { const split = value.trim().indexOf(' '); const program = split < 0 ? value.trim() : value.slice(0, split); const args = split < 0 ? '' : value.slice(split + 1); const result = await workbench.perform('Reviewing build command', async () => { await workbench.prepareCommand(commandSpec(program, args, '', '', false)); return true; }); if (result) setBuildDialog(false); }} />}{closeDialog && <ConfirmDialog title="Close AstraForge" label="Close and discard unsaved work" onClose={() => setCloseDialog(false)} onConfirm={async () => { await workbench.perform('Closing window', closeNativeWindow); }}>Closing interrupts active work and discards unsaved editor changes. Save your changes and stop active tasks before closing, or confirm to close now.</ConfirmDialog>}{ui.palette && <CommandPalette commands={commands} onClose={closePalette} />}{ui.settings && <SettingsDialog bindings={bindings} onBindings={setBindings} onClose={closeSettings} />}</div></WorkbenchContext.Provider>;
}
