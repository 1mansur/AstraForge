import { describe, expect, it } from 'vitest';
import { commandSpec, languageFor, parseArguments } from './commands';
describe('direct process argument parsing', () => {
  it('preserves Windows paths and quoted empty and spaced arguments', () => { expect(parseArguments('test "C:\\project files\\suite.ts" "" --watch=false')).toEqual(['test', 'C:\\project files\\suite.ts', '', '--watch=false']); });
  it('passes shell operators as literal arguments', () => { expect(parseArguments('status && erase file')).toEqual(['status', '&&', 'erase', 'file']); });
  it('rejects incomplete quotes', () => { expect(() => parseArguments('"unfinished')).toThrow('Unclosed quote'); });
  it('constructs unapproved commands and validates environment names', () => { expect(commandSpec('python', '-m pytest', 'tests', 'MODE=test\nX=a=b', true)).toEqual({ program: 'python', args: ['-m', 'pytest'], cwd: 'tests', env: { MODE: 'test', X: 'a=b' }, approved: false, isTest: true }); expect(() => commandSpec('python', '', '', 'BAD NAME=value', false)).toThrow('Invalid environment'); });
  it('rejects empty executable and malformed environment entries', () => { expect(() => commandSpec(' ', '', '', '', false)).toThrow('executable'); expect(() => commandSpec('git', '', '', 'SECRET', false)).toThrow('NAME=value'); });
  it('resolves Monaco languages without guessing unknown binary formats', () => { expect(languageFor('src/service.tsx')).toBe('typescript'); expect(languageFor('archive.blob')).toBe('plaintext'); });
});
