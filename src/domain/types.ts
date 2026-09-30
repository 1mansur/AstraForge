export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface AppError { code: string; message: string; category: string; recoverable: boolean; cause?: string | null; context: Json }
export interface Repository { id: string; root: string; name: string }
export interface FileEntry { path: string; name: string; kind: 'file' | 'directory'; size: number }
export interface FileContent { path: string; content: string; hash: string; encoding: string }
export interface EditorDocument extends FileContent { draft: string; conflict: boolean; line?: number }
export type SearchMode = 'text' | 'regex' | 'symbol' | 'filename' | 'semantic';
export interface SearchHit { path: string; line: number; column: number; content: string; symbol: string | null }
export interface IndexStats { files: number; symbols: number; dependencies: number; durationMs: number; cancelled: boolean }
export interface GraphEdge { source: string; target: string; kind: string }
export interface GitEntry { path: string; index: string; worktree: string }
export interface GitStatus { branch: string; entries: GitEntry[] }
export interface PatchProposal { path: string; content: string | null; expectedHash: string | null }
export interface PatchChange { path: string; before: string | null; after: string | null; originalHash: string | null; resultHash: string | null; diff: string }
export interface PatchSet { id: string; repositoryId: string; status: string; source: string; createdAt: number; changes: PatchChange[] }
export interface CommandSpec { program: string; args: string[]; cwd: string | null; env: Record<string, string>; approved: boolean; isTest: boolean }
export interface PolicyDecision { level: string; reason: string }
export interface OutputChunk { sequence: number; stream: string; text: string }
export interface ProcessSnapshot { id: string; chunks: OutputChunk[]; nextCursor: number; exitCode: number | null; running: boolean; truncated: boolean }
export interface ProcessSession { id: string; label: string; spec: CommandSpec; chunks: OutputChunk[]; cursor: number; running: boolean; drained: boolean; exitCode: number | null; truncated: boolean; startedAt: number }
export interface Settings { theme: 'dark' | 'light' | 'system'; provider: { endpoint: string; model: string; apiKeyEnv: string; embeddingModel?: string }; maxAgentSteps: number; contextBudget: number; testProgram: string; testArgs: string[] }
export interface TraceNode { id: string; kind: string; label: string; status: string; startedAt: number; durationMs: number; detail: string }
export interface AgentSession { id: string; task: string; status: string; createdAt: number; nodes: TraceNode[]; edges: { source: string; target: string }[]; summary: string; pendingApproval?: string; pendingTool?: Json; pendingCommand?: CommandSpec }
export interface Methods {
  repositories: { input: Record<string, never>; output: Repository[] };
  open_repository: { input: { path: string }; output: Repository };
  tree: { input: { path: string }; output: FileEntry[] };
  read_file: { input: { path: string }; output: FileContent };
  write_file: { input: { path: string; content: string; expectedHash: string }; output: FileContent };
  create_file: { input: { path: string; directory: boolean }; output: null };
  remove_file: { input: { path: string; expectedHash?: string }; output: null };
  rename_file: { input: { from: string; to: string }; output: null };
  index_repository: { input: Record<string, never>; output: IndexStats };
  cancel_index: { input: Record<string, never>; output: null };
  cancel_search: { input: Record<string, never>; output: null };
  search: { input: { query: string; mode: SearchMode; offset: number; limit: number }; output: SearchHit[] };
  graph: { input: { path: string; direction: string }; output: GraphEdge[] };
  health: { input: Record<string, never>; output: Json };
  git_status: { input: Record<string, never>; output: GitStatus };
  git_diff: { input: { path?: string; staged: boolean }; output: string };
  git_history: { input: { path?: string }; output: Json };
  git_branches: { input: Record<string, never>; output: string[] };
  git_action: { input: { action: string; value: string }; output: string };
  propose_patch: { input: { proposals: PatchProposal[]; source: string }; output: PatchSet };
  patches: { input: Record<string, never>; output: PatchSet[] };
  apply_patch: { input: { id: string }; output: PatchSet };
  reject_patch: { input: { id: string }; output: PatchSet };
  revert_patch: { input: { id: string }; output: PatchSet };
  classify_command: { input: { spec: CommandSpec }; output: PolicyDecision };
  start_command: { input: { spec: CommandSpec }; output: string };
  poll_command: { input: { id: string; cursor: number }; output: ProcessSnapshot };
  command_input: { input: { id: string; text: string }; output: null };
  stop_command: { input: { id: string }; output: null };
  settings: { input: Record<string, never>; output: Settings };
  save_settings: { input: { settings: Settings }; output: null };
  start_agent: { input: { task: string; mode: 'task' | 'review' }; output: AgentSession };
  agent_session: { input: { id: string }; output: AgentSession };
  agent_sessions: { input: Record<string, never>; output: AgentSession[] };
  stop_agent: { input: { id: string }; output: null };
  continue_agent: { input: { id: string }; output: AgentSession };
  approve_agent_command: { input: { id: string }; output: AgentSession };
  export_session: { input: { id: string }; output: string };
  import_session: { input: { json: string }; output: AgentSession };
}
