import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../App';
import { Workbench } from '../state/workbench';
import { TestBridge } from '../test/bridge';
import { WorkbenchContext } from './context';
import { TerminalPanel } from './TerminalPanel';
import { CommandPalette } from './CommandPalette';
import { GraphPanel, SearchPanel } from './SearchPanel';
vi.mock('@monaco-editor/react', () => ({ default: () => <div aria-label="Test editor" /> }));
describe('desktop boundary', () => {
  it('shows an honest browser-only state and never fabricates repositories', async () => { const bridge = new TestBridge(); bridge.available = false; render(<App workbench={new Workbench(bridge)} />); expect(await screen.findByText('Desktop application required')).toBeInTheDocument(); expect(screen.getByRole('button', { name: /^Open repository$/ })).toBeDisabled(); expect(screen.getByText('No repository open')).toBeInTheDocument(); expect(bridge.calls).toEqual([]); });
  it('supports keyboard command palette navigation and runs the selected action', () => { const open = vi.fn(); const settings = vi.fn(); const close = vi.fn(); render(<CommandPalette commands={[{ id: 'open', label: 'Open Repository', detail: 'Open real files', run: open }, { id: 'settings', label: 'Open Settings', detail: 'Provider configuration', run: settings }]} onClose={close} />); const search = screen.getByRole('textbox', { name: 'Search commands' }); fireEvent.change(search, { target: { value: 'settings' } }); fireEvent.keyDown(search, { key: 'Enter' }); expect(settings).toHaveBeenCalledOnce(); expect(open).not.toHaveBeenCalled(); expect(close).toHaveBeenCalledOnce(); });
});
describe('command confirmation UI', () => {
  it('shows actual command intent and dispatches only after explicit approval', async () => {
    const bridge = new TestBridge({ start_command: () => 'command-1' }); const workbench = new Workbench(bridge);
    workbench.repository.patch({ current: { id: 'repo', name: 'example', root: 'C:\\example' } });
    workbench.terminal.patch({ pending: { spec: { program: 'python', args: ['-m', 'pytest'], cwd: 'tests', env: {}, approved: false, isTest: true }, policy: { level: 'CAUTION', reason: 'Repository code will execute' } } });
    render(<WorkbenchContext.Provider value={workbench}><TerminalPanel /></WorkbenchContext.Provider>);
    expect(screen.getByText('Repository code will execute')).toBeInTheDocument(); expect(screen.getByText('python "-m" "pytest"')).toBeInTheDocument(); expect(bridge.calls).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Approve and run' }));
    await waitFor(() => expect(bridge.calls[0]).toMatchObject({ method: 'start_command', params: { spec: { approved: true, cwd: 'tests' } } })); expect(await screen.findByText('Running')).toBeInTheDocument();
  });
});
describe('graph source navigation', () => {
  it('does not offer file navigation for unresolved external modules', () => {
    const bridge = new TestBridge(); const workbench = new Workbench(bridge); workbench.search.patch({ direction: 'dependencies', graphPath: 'src/main.ts', graph: [{ source: 'src/main.ts', target: 'module:react', kind: 'imports' }] });
    render(<WorkbenchContext.Provider value={workbench}><GraphPanel /></WorkbenchContext.Provider>);
    expect(screen.getByRole('button', { name: 'module:react' })).toBeDisabled(); expect(bridge.calls).toEqual([]);
  });
  it.each([
    { direction: 'references', source: 'src/caller.ts:42', target: 'resolveToken', expected: 'src/caller.ts', line: 42 },
    { direction: 'callers', source: 'src/caller.ts:8', target: 'resolveToken', expected: 'src/caller.ts', line: 8 },
    { direction: 'dependents', source: 'src/dependent.ts', target: 'src/module.ts', expected: 'src/dependent.ts', line: undefined },
    { direction: 'affected', source: 'src/affected.ts', target: 'src/module.ts', expected: 'src/affected.ts', line: undefined },
    { direction: 'dependencies', source: 'src/caller.ts', target: 'src/module.ts', expected: 'src/module.ts', line: undefined },
  ])('opens the correct file and source line for $direction', async ({ direction, source, target, expected, line }) => {
    const bridge = new TestBridge({ read_file: ({ path }) => ({ path, content: 'source', hash: 'hash', encoding: 'utf-8' }) }); const workbench = new Workbench(bridge); workbench.search.patch({ direction, graphPath: 'query', graph: [{ source, target, kind: direction }] });
    render(<WorkbenchContext.Provider value={workbench}><GraphPanel /></WorkbenchContext.Provider>);
    fireEvent.click(screen.getByRole('button', { name: line ? `${expected}:${line}` : expected }));
    await waitFor(() => expect(workbench.editor.get().active).toBe(expected));
    expect(bridge.calls).toEqual([{ method: 'read_file', params: { path: expected } }]); expect(workbench.editor.get().documents[0]?.line).toBe(line);
  });
});
describe('search result bounds', () => {
  it.each([{ mode: 'semantic' as const, more: false }, { mode: 'text' as const, more: true }])('offers further pages only when supported in $mode mode', async ({ mode, more }) => {
    const bridge = new TestBridge({ search: () => Array.from({ length: 100 }, (_, index) => ({ path: `src/file-${index}.ts`, line: 1, column: 1, content: 'match', symbol: null })) }); const workbench = new Workbench(bridge); workbench.repository.patch({ current: { id: 'repo', root: 'C:\\repo', name: 'repo' } }); workbench.search.patch({ mode });
    render(<WorkbenchContext.Provider value={workbench}><SearchPanel /></WorkbenchContext.Provider>);
    fireEvent.change(screen.getByRole('textbox', { name: 'Search repository' }), { target: { value: 'match' } }); fireEvent.click(screen.getByRole('button', { name: 'Search' }));
    await waitFor(() => expect(workbench.search.get().hits).toHaveLength(100));
    expect(workbench.search.get().more).toBe(more);
    if (more) expect(screen.getByRole('button', { name: 'Load next 100 results' })).toBeInTheDocument();
    else { expect(screen.queryByRole('button', { name: 'Load next 100 results' })).not.toBeInTheDocument(); expect(screen.getByText('Up to 100 ranked results · 100 shown')).toBeInTheDocument(); }
  });
});
