import type { GraphEdge } from '../domain/types';
export interface SourceLocation { path: string; line?: number; label: string; external?: boolean }
export function graphLocation(edge: GraphEdge, direction: string): SourceLocation {
  const sourceDirections = ['references', 'callers', 'dependents', 'incoming', 'affected'];
  const label = sourceDirections.includes(direction) ? edge.source : edge.target;
  if (label.startsWith('module:')) return { path: label, label, external: true };
  const match = /^(.*):([1-9]\d*)$/.exec(label);
  if (match?.[1] && match[2]) { const line = Number(match[2]); if (Number.isSafeInteger(line)) return { path: match[1], line, label }; }
  return { path: label, label };
}
