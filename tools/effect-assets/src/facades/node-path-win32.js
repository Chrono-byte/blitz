import { posix, win32 } from "blitz-internal:node-path-shared";

export const resolve = win32.resolve;
export const normalize = win32.normalize;
export const isAbsolute = win32.isAbsolute;
export const join = win32.join;
export const relative = win32.relative;
export const toNamespacedPath = win32.toNamespacedPath;
export const dirname = win32.dirname;
export const basename = win32.basename;
export const extname = win32.extname;
export const format = win32.format;
export const parse = win32.parse;
export const matchesGlob = win32.matchesGlob;
export const sep = win32.sep;
export const delimiter = win32.delimiter;
export const _makeLong = win32._makeLong;

export { posix, win32 };
export default win32;
