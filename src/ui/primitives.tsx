import { useEffect, useRef, useState } from 'react';
import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { X, AlertTriangle, LoaderCircle } from 'lucide-react';
export function IconButton({ label, children, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) { return <button type="button" className="icon-button" title={label} aria-label={label} {...props}>{children}</button>; }
export function EmptyState({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) { return <div className="empty-state">{icon}<strong>{title}</strong>{children && <div>{children}</div>}</div>; }
export function Busy({ label }: { label: string }) { return <span className="busy"><LoaderCircle size={13} className="spin" />{label}</span>; }
export function Modal({ title, children, onClose, wide = false }: { title: string; children: ReactNode; onClose: () => void; wide?: boolean }) {
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    const element = dialog.current;
    element?.querySelector<HTMLElement>('input,textarea,select,button')?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.stopPropagation(); onClose(); }
      if (event.key === 'Tab' && element) {
        const focusable = Array.from(element.querySelectorAll<HTMLElement>('button:not([disabled]),input:not([disabled]),textarea:not([disabled]),select:not([disabled]),[tabindex="0"]'));
        const first = focusable[0]; const last = focusable.at(-1);
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    };
    document.addEventListener('keydown', keydown);
    return () => { document.removeEventListener('keydown', keydown); if (previous instanceof HTMLElement) previous.focus(); };
  }, [onClose]);
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) onClose(); }}><div className={`modal ${wide ? 'modal-wide' : ''}`} role="dialog" aria-modal="true" aria-label={title} ref={dialog}><div className="modal-heading"><h2>{title}</h2><IconButton label="Close dialog" onClick={onClose}><X size={17} /></IconButton></div>{children}</div></div>;
}
export function InputDialog({ title, label, initial = '', submit = 'Continue', destructive = false, onConfirm, onClose }: { title: string; label: string; initial?: string; submit?: string; destructive?: boolean; onConfirm: (value: string) => Promise<void>; onClose: () => void }) {
  const [value, setValue] = useState(initial);
  const [pending, setPending] = useState(false);
  return <Modal title={title} onClose={onClose}><form onSubmit={event => { event.preventDefault(); setPending(true); void onConfirm(value).finally(() => setPending(false)); }}><label className="field">{label}<input value={value} onChange={event => setValue(event.target.value)} required autoComplete="off" spellCheck={false} /></label><div className="modal-actions"><button type="button" onClick={onClose}>Cancel</button><button className={destructive ? 'danger' : 'primary'} disabled={pending || !value.trim()}>{pending ? 'Working…' : submit}</button></div></form></Modal>;
}
export function ConfirmDialog({ title, children, label = 'Confirm', onConfirm, onClose }: { title: string; children: ReactNode; label?: string; onConfirm: () => Promise<void> | void; onClose: () => void }) {
  const [pending, setPending] = useState(false);
  return <Modal title={title} onClose={onClose}><div className="confirm-copy"><AlertTriangle size={20} />{children}</div><div className="modal-actions"><button onClick={onClose}>Cancel</button><button className="danger" disabled={pending} onClick={() => { setPending(true); void Promise.resolve(onConfirm()).finally(() => setPending(false)); }}>{pending ? 'Working…' : label}</button></div></Modal>;
}
