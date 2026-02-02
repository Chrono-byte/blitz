/**
 * Standard library: console.
 * Written in TypeScript, runs on the Rust VM.
 * The VM provides a minimal native hook (e.g. __native_print); we wrap it here.
 */

import * as host from "../host.js";

// Resolve native print hook from host if present; fall back to console.log
const __native_print: any = host.native_print
  ? host.native_print
  : function (..._args: any[]) {
      /* noop */
    };

/** Print all args joined with spaces (JS-like behavior). */
export function log(...args: unknown[]): void {
  const out = args.map((a) => String(a)).join(" ");
  if (out !== "") __native_print(out);
}

export function warn(...args: unknown[]): void {
  const out = args.map((a) => String(a)).join(" ");
  if (out !== "") __native_print(out);
}

export function error(...args: unknown[]): void {
  const out = args.map((a) => String(a)).join(" ");
  if (out !== "") __native_print(out);
}
