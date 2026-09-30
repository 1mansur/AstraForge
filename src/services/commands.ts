import type { CommandSpec } from '../domain/types';
export function formatArguments(args: string[]): string { return args.map(arg => `'${arg.replaceAll("'", "'\"'\"'")}'`).join(' '); }
export function parseCommand(input: string): CommandSpec { const [program, ...args] = parseArguments(input); if (!program) throw new Error('Enter an executable to run.'); return { program, args, cwd: null, env: {}, approved: false, isTest: false }; }
export function parseArguments(input: string): string[] {
  let current = '';
  let quote = '';
  let active = false;
  const result: string[] = [];
  for (const character of input) {
    if (quote) {
      if (character === quote) quote = ''; else current += character;
    } else if (character === '"' || character === "'") { quote = character; active = true; }
    else if (/\s/.test(character)) { if (active) { result.push(current); current = ''; active = false; } }
    else { current += character; active = true; }
  }
  if (quote) throw new Error('Unclosed quote in command arguments.');
  if (active) result.push(current);
  return result;
}
export function commandSpec(program: string, args: string, cwd: string, env: string, isTest: boolean): CommandSpec {
  if (!program.trim()) throw new Error('Enter an executable to run.');
  const environment: Record<string, string> = {};
  for (const line of env.split('\n').filter(line => line.trim())) {
    const split = line.indexOf('=');
    if (split < 1) throw new Error('Environment variables use NAME=value, one per line.');
    const key = line.slice(0, split).trim();
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) throw new Error(`Invalid environment variable name: ${key}`);
    environment[key] = line.slice(split + 1);
  }
  return { program: program.trim(), args: parseArguments(args), cwd: cwd.trim() || null, env: environment, approved: false, isTest };
}
export function languageFor(path: string): string {
  const extension = path.split('.').pop()?.toLowerCase();
  const languages: Record<string, string> = { ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript', rs: 'rust', py: 'python', json: 'json', md: 'markdown', css: 'css', html: 'html', yaml: 'yaml', yml: 'yaml', toml: 'ini', sh: 'shell', ps1: 'powershell', sql: 'sql', c: 'c', cpp: 'cpp', go: 'go', java: 'java', txt: 'plaintext' };
  return languages[extension ?? ''] ?? 'plaintext';
}
