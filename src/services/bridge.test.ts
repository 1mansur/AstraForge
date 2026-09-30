import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import fixtures from '../../fixtures/protocol-v1.json';
import { desktopBridge } from './bridge';
import { decodeEvent } from './events';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: () => true }));
describe('Rust and desktop protocol golden examples', () => {
  beforeEach(() => { vi.mocked(invoke).mockReset(); vi.mocked(invoke).mockResolvedValue(null); });
  it('serializes a repository request using the same envelope as the native contract', async () => { await desktopBridge.request('read_file', fixtures.scopedRequest.request.params, fixtures.scopedRequest.repositoryId); expect(invoke).toHaveBeenCalledExactlyOnceWith('request', { envelope: fixtures.scopedRequest }); });
  it('serializes a global request with an explicit null repository', async () => { await desktopBridge.request('repositories', {}); expect(invoke).toHaveBeenCalledExactlyOnceWith('request', { envelope: fixtures.globalRequest }); });
  it('decodes both native event examples without inventing fields or dropping sequence information', () => { expect(decodeEvent('workspace_changed', fixtures.workspaceEvent)).toEqual(fixtures.workspaceEvent); expect(decodeEvent('diagnostic', fixtures.diagnosticEvent)).toEqual(fixtures.diagnosticEvent); });
});
