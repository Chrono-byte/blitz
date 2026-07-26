import { posix, win32 } from "blitz-internal:node-path-shared";

export const resolve = posix.resolve;
export const normalize = posix.normalize;
export const isAbsolute = posix.isAbsolute;
export const join = posix.join;
export const relative = posix.relative;
export const toNamespacedPath = posix.toNamespacedPath;
export const dirname = posix.dirname;
export const basename = posix.basename;
export const extname = posix.extname;
export const format = posix.format;
export const parse = posix.parse;
export const matchesGlob = posix.matchesGlob;
export const sep = posix.sep;
export const delimiter = posix.delimiter;
export const _makeLong = posix._makeLong;

export { posix, win32 };
export default posix;
