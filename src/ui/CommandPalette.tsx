import { useEffect, useRef, useState } from 'react';
import { Command, ArrowRight } from 'lucide-react';
import { Modal } from './primitives';
export interface PaletteCommand { id: string; label: string; detail: string; shortcut?: string; run: () => void; disabled?: boolean }
export function CommandPalette({ commands, onClose }: { commands: PaletteCommand[]; onClose: () => void }) {
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const matches = commands.filter(command => !command.disabled && `${command.label} ${command.detail}`.toLowerCase().includes(query.toLowerCase()));
  useEffect(() => { list.current?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' }); }, [selected]);
  const run = (command: PaletteCommand) => { onClose(); command.run(); };
  return <Modal title="Command palette" onClose={onClose}><div className="palette-input"><Command size={16} /><input aria-label="Search commands" placeholder="Type a command…" value={query} onChange={event => { setQuery(event.target.value); setSelected(0); }} onKeyDown={event => { if (event.key === 'ArrowDown') { event.preventDefault(); setSelected(index => Math.min(index + 1, matches.length - 1)); } if (event.key === 'ArrowUp') { event.preventDefault(); setSelected(index => Math.max(0, index - 1)); } if (event.key === 'Enter') { event.preventDefault(); const command = matches[selected]; if (command) run(command); } }} /></div><div className="palette-list" role="listbox" aria-label="Commands" ref={list}>{matches.map((command, index) => <button role="option" aria-selected={index === selected} className={index === selected ? 'selected' : ''} key={command.id} onMouseMove={() => setSelected(index)} onClick={() => run(command)}><ArrowRight size={13} /><span>{command.label}<small>{command.detail}</small></span>{command.shortcut && <kbd>{command.shortcut}</kbd>}</button>)}{!matches.length && <p className="panel-padding muted">No matching commands.</p>}</div><div className="palette-footer"><span>↑↓ navigate</span><span>↵ run</span><span>esc close</span></div></Modal>;
}
