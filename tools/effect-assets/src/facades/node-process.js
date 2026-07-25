const process = globalThis.process;

export const arch = process.arch;
export const argv = process.argv;
export const cwd = process.cwd;
export const env = process.env;
export const execPath = process.execPath;
export const platform = process.platform;
export const version = process.version;
export const versions = process.versions;

export default process;
