import { act, fireEvent, render, screen } from '@testing-library/react';
import { expect, it } from 'vitest';
import { Workbench } from '../state/workbench';
import { TestBridge } from '../test/bridge';
import { WorkbenchContext } from './context';
import { GitDiff, GitPanel } from './GitPanel';
import { DiffText } from './DiffText';
import { VirtualList } from './VirtualList';
it('bounds mounted diff rows independently of repository line count', () => {
  const workbench = new Workbench(new TestBridge()); const lines = 10000; workbench.git.patch({ diff: '+x\n'.repeat(lines) }); const started = performance.now(); const result = render(<WorkbenchContext.Provider value={workbench}><GitDiff /></WorkbenchContext.Provider>); const count = result.container.querySelectorAll('.diff-added').length;
  process.stdout.write(`${JSON.stringify({ profile: 'diff-mount', lines, mountedRows: count, mountMs: performance.now() - started })}\n`); expect(count).toBeGreaterThan(0); expect(count).toBeLessThan(100);
});
it('keeps very large diffs navigable while displaying repository content as text', () => {
  const text = `${'<img src=x onerror=attack()>\n'.repeat(200000)}+last line`; const result = render(<DiffText text={text} />); const viewport = screen.getByLabelText('Unified diff'); expect(result.container.querySelectorAll('.diff-line').length).toBeLessThan(100); expect(result.container.querySelector('img')).toBeNull(); fireEvent.scroll(viewport, { target: { scrollTop: 200000 * 20 } }); expect(screen.getByText('+last line')).toBeInTheDocument(); result.rerender(<DiffText text="+short replacement" />); expect(screen.getByText('+short replacement')).toBeInTheDocument();
});
it('bounds mounted Git change rows for a ten-thousand-file worktree', () => {
  const workbench = new Workbench(new TestBridge()); workbench.repository.patch({ current: { id: 'repo', root: 'C:/repo', name: 'repo' } }); workbench.git.patch({ status: { branch: 'main', entries: Array.from({ length: 10000 }, (_, index) => ({ path: `file-${index}.ts`, index: ' ', worktree: 'M' })) } }); const result = render(<WorkbenchContext.Provider value={workbench}><GitPanel /></WorkbenchContext.Provider>); expect(result.container.querySelectorAll('.git-file').length).toBeGreaterThan(0); expect(result.container.querySelectorAll('.git-file').length).toBeLessThan(100);
});
it('survives batched scrolling and shrinking results without retaining a synthetic event', () => {
  const items = Array.from({ length: 10000 }, (_, index) => `${index}`); const view = (values: string[]) => <VirtualList items={values} rowHeight={20} label="Virtual rows" itemKey={value => value} render={value => <span>{value}</span>} />; const result = render(view(items)); const viewport = screen.getByLabelText('Virtual rows'); act(() => { fireEvent.scroll(viewport, { target: { scrollTop: 10000 } }); fireEvent.scroll(viewport, { target: { scrollTop: 15000 } }); }); expect(result.container.querySelectorAll('span').length).toBeLessThan(100); result.rerender(view(['replacement'])); expect(screen.getByText('replacement')).toBeInTheDocument();
});
