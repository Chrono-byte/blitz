/*
 * Simple console probe script
 *
 * Purpose: exercise the runtime's console / native print bindings using a few
 * different fallbacks so output is visible even when `console.log` is broken.
 *
 * Usage: `cargo run -- examples/probe_simple.ts`
 */

const g: any = typeof globalThis !== "undefined" ? (globalThis as any) : {};

function probeLog(msg: string): void {
    // Try direct host/native printers first (many environments publish helpers).
    try {
        const p: any =
            g.__native_print ||
            g.__native_print_raw ||
            g.native_print ||
            (g.host && g.host.native_print) ||
            g.host_print ||
            undefined;
        if (typeof p === "function") {
            try {
                p(msg);
                return;
            } catch (_) { }
        }
    } catch (_) { }

    // Next try host-provided console
    try {
        if (g.console && typeof g.console.log === "function") {
            try {
                g.console.log(msg);
                return;
            } catch (_) { }
        }
    } catch (_) { }

    // Last resort: try the local console (may be unavailable in some hosts)
    try {
        console.log(msg);
    } catch (_) { }
}

function runProbe(): void {
    probeLog("probe: started");
    try { probeLog("probe: typeof globalThis = " + typeof globalThis); } catch (_) { }
    try { probeLog("probe: global keys = " + Object.keys((globalThis as any) || {}).join(", ")); } catch (_) { }
    try { probeLog("probe: typeof console = " + typeof g.console); } catch (_) { }
    try { probeLog("probe: typeof console.log = " + typeof (g.console && g.console.log)); } catch (_) { }
    try { probeLog("probe: typeof __native_print = " + typeof g.__native_print); } catch (_) { }
    try { probeLog("probe: typeof host_print = " + typeof g.host_print); } catch (_) { }

    try {
        probeLog("probe: calling console.log('hello world')...");
        try {
            (g.console as any).log("hello world");
            probeLog("probe: console.log call returned");
        } catch (e) {
            probeLog("probe: console.log threw/failed: " + (e && (e as any).message ? (e as any).message : String(e)));
        }
    } catch (_) { }

    try {
        probeLog("probe: calling __native_print('hello world native')...");
        try {
            const fn: any = g.__native_print || g.__native_print_raw || g.native_print || (g.host && g.host.native_print) || g.host_print;
            if (typeof fn === "function") {
                try {
                    fn("hello world (from __native_print)");
                    probeLog("probe: __native_print call returned");
                } catch (e) {
                    probeLog("probe: __native_print threw: " + String(e));
                }
            } else {
                probeLog("probe: no callable native printer found (typeof=" + typeof fn + ")");
            }
        } catch (e) {
            probeLog("probe: __native_print test failed: " + String(e));
        }
    } catch (_) { }

    // Try to replace console.log with a JS wrapper and exercise the assignment.
    try {
        const wrapped: any = function (...args: any[]): void {
            try {
                probeLog("wrapped console.log -> " + args.map((a: any) => String(a)).join(" "));
            } catch (_) { }
        };
        try {
            g.console.log = wrapped;
            probeLog("probe: assigned g.console.log = wrapped");
        } catch (e) {
            probeLog("probe: assign console.log failed: " + String(e));
        }
        try {
            (g.console as any).log("after replace");
            probeLog("probe: replaced console.log call returned");
        } catch (e) {
            probeLog("probe: replaced console.log threw: " + String(e));
        }
    } catch (_) { }

    probeLog("probe: done");
}

// Run it
runProbe();
