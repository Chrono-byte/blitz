import { resolve } from 'node:path';
import { $$ } from '../../util.js';

const executableName = process.platform === 'win32' ? 'blitz.exe' : 'blitz';
const defaultRoot = resolve(import.meta.dirname, '..', '..', '..', 'blitz');
const binary = process.env.BLITZ_BIN
  ?? resolve(process.env.BLITZ_ROOT ?? defaultRoot, 'target', 'release', executableName);

export default (file, module = false) => {
  const args = ['--test262-harness'];
  if (module) args.push('--module');
  args.push(file);
  return $$(binary, args);
};
