import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export const blitzRoot = resolve(
  process.env.BLITZ_ROOT ?? resolve(import.meta.dirname, '..', '..', '..', 'blitz'),
);

const executableName = process.platform === 'win32' ? 'blitz.exe' : 'blitz';
const test262Name = process.platform === 'win32' ? 'test262.exe' : 'test262';

export const blitzBin = resolve(blitzRoot, 'target', 'release', executableName);
export const test262Bin = resolve(blitzRoot, 'target', 'release', test262Name);

const run = (command, args, options = {}) => {
  const result = spawnSync(command, args, {
    cwd: blitzRoot,
    encoding: 'utf8',
    stdio: options.stdio ?? 'pipe',
  });
  if (result.status !== 0) {
    throw new Error(
      `${command} ${args.join(' ')} failed\n${result.stdout ?? ''}${result.stderr ?? ''}`,
    );
  }
  return (result.stdout ?? '').trim();
};

export default async () => {
  if (!existsSync(resolve(blitzRoot, 'Cargo.toml'))) {
    throw new Error(`BLITZ_ROOT does not point to a Blitz checkout: ${blitzRoot}`);
  }

  if (process.env.BLITZ_SKIP_BUILD !== '1') {
    run('cargo', ['build', '--release', '--bin', 'blitz', '--bin', 'test262'], {
      stdio: 'inherit',
    });
  }

  if (!existsSync(blitzBin) || !existsSync(test262Bin)) {
    throw new Error(`Blitz release binaries were not found under ${resolve(blitzRoot, 'target', 'release')}`);
  }

  process.env.BLITZ_BIN = blitzBin;
  process.env.BLITZ_TEST262_BIN = test262Bin;

  const version = run('git', ['rev-parse', '--short=7', 'HEAD']);
  return { version };
};
