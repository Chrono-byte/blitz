/*
 * Tiny test that calls the VM-provided native printer directly.
 * This script tries several well-known host hooks in order and
 * falls back to `console.log` when necessary so the test remains
 * useful even in environments where the native hook is missing.
 *
 * Usage:
 *   cargo run -- examples/test_print.ts
 */

const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : {};

try {
  // Prefer direct host-provided native print entrypoints when available.
  const printer: any =
    g.__native_print ||
    g.__native_print_raw ||
    g.native_print ||
    (g.host && g.host.native_print) ||
    g.host_print;

  if (typeof printer === "function") {
    try {
      // Call the native printer directly. Use a short, distinctive message.
      printer("test_print: hello from __native_print");
    } catch (e) {
      // If the host printer throws, fall back to console so the error is visible.
      try {
        console.log("test_print: native printer threw: " + String(e));
      } catch (_) {}
    }
  } else {
    // No native printer available: attempt to use host console or console.log.
    try {
      if (g.console && typeof g.console.log === "function") {
        g.console.log("test_print: fallback to console.log (via host console)");
      } else {
        // Last resort; may also be broken in some bootstrap orders.
        console.log("test_print: no native printer available");
      }
    } catch (_) {}
  }
} catch (e) {
  try {
    console.log("test_print: unexpected error: " + String(e));
  } catch (_) {}
}
