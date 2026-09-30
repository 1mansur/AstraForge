import { useEffect, useMemo, useRef, useState } from 'react';
function lineOffsets(text: string): Uint32Array {
  let count = 1; for (let index = 0; index < text.length; index++) if (text.charCodeAt(index) === 10) count++;
  const offsets = new Uint32Array(count); let line = 1;
  for (let index = 0; index < text.length; index++) if (text.charCodeAt(index) === 10) offsets[line++] = index + 1;
  return offsets;
}
export function DiffText({ text, preview = false }: { text: string; preview?: boolean }) {
  const offsets = useMemo(() => lineOffsets(text), [text]); const container = useRef<HTMLDivElement>(null); const [range, setRange] = useState({ top: 0, height: 600 }); const rowHeight = 20;
  useEffect(() => { const element = container.current; if (!element) return; const observer = new ResizeObserver(entries => { const entry = entries[0]; if (entry) setRange(previous => ({ ...previous, height: entry.contentRect.height })); }); observer.observe(element); return () => observer.disconnect(); }, []);
  useEffect(() => { if (container.current) container.current.scrollTop = 0; setRange(previous => ({ ...previous, top: 0 })); }, [text]);
  const start = Math.min(Math.max(0, offsets.length - 1), Math.max(0, Math.floor(range.top / rowHeight) - 6)); const end = Math.min(offsets.length, Math.max(start + 1, Math.ceil((range.top + range.height) / rowHeight) + 6));
  return <div className={`diff-window ${preview ? 'patch-preview' : ''}`} ref={container} aria-label="Unified diff" tabIndex={0} onScroll={event => { const top = event.currentTarget.scrollTop; setRange(previous => ({ ...previous, top })); }}><div style={{ height: offsets.length * rowHeight, minWidth: '100%', position: 'relative' }}>{Array.from({ length: end - start }, (_, offset) => {
    const index = start + offset; const line = text.slice(offsets[index], index + 1 < offsets.length ? (offsets[index + 1] ?? text.length + 1) - 1 : text.length); const type = line.startsWith('+') && !line.startsWith('+++') ? 'diff-added' : line.startsWith('-') && !line.startsWith('---') ? 'diff-removed' : line.startsWith('@@') ? 'diff-header' : '';
    return <div key={index} className={`diff-line ${type}`} style={{ position: 'absolute', top: index * rowHeight, height: rowHeight }} data-line={index + 1}>{line || ' '}</div>;
  })}</div></div>;
}
