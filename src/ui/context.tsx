import { createContext, useContext, useSyncExternalStore } from 'react';
import type { Store } from '../state/store';
import type { Workbench } from '../state/workbench';
export const WorkbenchContext = createContext<Workbench | null>(null);
export function useWorkbench(): Workbench { const workbench = useContext(WorkbenchContext); if (!workbench) throw new Error('Workbench provider is missing.'); return workbench; }
export function useStore<T>(store: Store<T>): T { return useSyncExternalStore(store.subscribe, store.get, store.get); }
