import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
export function VirtualList<T>({ items, rowHeight, render, itemKey, label }: { items: T[]; rowHeight: number; render: (item: T, index: number) => ReactNode; itemKey: (item: T) => string; label: string }) {
  const container = useRef<HTMLDivElement>(null);
  const [range, setRange] = useState({ top: 0, height: 600 });
  useEffect(() => { const element = container.current; if (!element) return; const observer = new ResizeObserver(entries => { const entry = entries[0]; if (entry) setRange(previous => ({ ...previous, height: entry.contentRect.height })); }); observer.observe(element); return () => observer.disconnect(); }, []);
  const start = Math.min(Math.max(0, items.length - 1), Math.max(0, Math.floor(range.top / rowHeight) - 6));
  const end = Math.min(items.length, Math.max(start + 1, Math.ceil((range.top + range.height) / rowHeight) + 6));
  return <div ref={container} className="virtual-list" aria-label={label} onScroll={event => { const top = event.currentTarget.scrollTop; setRange(previous => ({ ...previous, top })); }}><div style={{ height: items.length * rowHeight, position: 'relative' }}>{items.slice(start, end).map((item, offset) => <div key={itemKey(item)} style={{ position: 'absolute', top: (start + offset) * rowHeight, left: 0, right: 0, height: rowHeight }}>{render(item, start + offset)}</div>)}</div></div>;
}
