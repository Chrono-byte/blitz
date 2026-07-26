import { defaultPath, posix, win32 } from "blitz-internal:node-path-shared";

export const resolve = defaultPath.resolve;
export const normalize = defaultPath.normalize;
export const isAbsolute = defaultPath.isAbsolute;
export const join = defaultPath.join;
export const relative = defaultPath.relative;
export const toNamespacedPath = defaultPath.toNamespacedPath;
export const dirname = defaultPath.dirname;
export const basename = defaultPath.basename;
export const extname = defaultPath.extname;
export const format = defaultPath.format;
export const parse = defaultPath.parse;
export const matchesGlob = defaultPath.matchesGlob;
export const sep = defaultPath.sep;
export const delimiter = defaultPath.delimiter;
export const _makeLong = defaultPath._makeLong;

export { posix, win32 };
export default defaultPath;
