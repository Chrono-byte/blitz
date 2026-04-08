// Diagnostic probe for console / native print bindings
// Prints a few diagnostic lines using the host_print shim so output
// is visible even when console.log is broken.
function hp(msg: string): void {
  try {
    const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : undefined;
    const p = g ? g.host_print || g.__native_print || (g as any).native_print : undefined;
    if (typeof p === "function") {
      try {
        p(msg);
        return;
      } catch (_) {}
    }
  } catch (_) {}
  // last resort: try console directly (may fail)
  try {
    console.log(msg);
  } catch (_) {}
}

hp("probe: typeof globalThis = " + typeof globalThis);
try {
  hp("probe: global keys = " + Object.keys((globalThis as any) || {}).join(", "));
} catch (_) {}
const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : {};
try {
  hp("probe: typeof console = " + typeof g.console);
} catch (_) {}
try {
  hp("probe: typeof console.log = " + typeof (g.console && g.console.log));
} catch (_) {}
try {
  hp("probe: typeof __native_print = " + typeof g.__native_print);
} catch (_) {}
try {
  hp("probe: calling console.log('hello world')...");
  try {
    (g.console as any).log("hello world");
    hp("probe: console.log call returned");
  } catch (e) {
    hp("probe: console.log threw/failed: " + (e && e.message ? e.message : String(e)));
  }
} catch (_) {}

// Try to replace console.log with a soft wrapper
try {
  const wrapped = function (...args: any[]) {
    hp("wrapped console.log -> " + args.map((a: any) => String(a)).join(" "));
  };
  try {
    g.console.log = wrapped;
    hp("probe: assigned g.console.log = wrapped");
  } catch (e) {
    hp("probe: assign console.log failed: " + String(e));
  }
  try {
    (g.console as any).log("after replace");
  } catch (e) {
    hp("probe: replaced console.log threw: " + String(e));
  }
} catch (_) {}

hp("probe: done");
