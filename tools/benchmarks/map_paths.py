#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Compare map allocation and execution paths with checked results."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import statistics
import tempfile

from compare import ROOT, FIXTURES, run


def cases():
    small = (FIXTURES / "array_map.js").read_text()
    fresh = "globalThis.benchmark = function() {\n" + small + "\nreturn squares[9] + squares.length; };"
    reused = "var square = function(v) { return v * v; };\n" + fresh.replace(
        "function (v) {\n    return v * v;\n  }", "square"
    )
    if reused.endswith(fresh):
        raise ValueError("small-map callback fixture changed")
    setup = "var values=[]; for(var i=0; i<100000; i=i+1) values.push(i);\n"
    mapped = "var mapped=values.map(function(v) { return v*v; });\n"
    return {
        "small_fresh": (fresh, 110),
        "small_reused": (reused, 110),
        "large_precreated": (
            setup + "globalThis.benchmark=function() {" + mapped + "return mapped.length+mapped[99999]; };",
            9999900001,
        ),
        "large_chain": (
            setup + "globalThis.benchmark=function() {" + mapped
            + "var next=mapped.map(function(v) { return v+1; }); return next.length+next[99999]; };",
            9999900002,
        ),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, default=ROOT / "target/release/examples/engine_benchmark")
    parser.add_argument("--warmups", type=int, default=10)
    parser.add_argument("--samples", type=int, default=51)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.warmups < 0 or args.samples < 1:
        parser.error("warmups must be nonnegative and samples positive")
    engines = {"before": [str(args.before.resolve())], "after": [str(args.after.resolve())]}
    for name in ("node", "bun"):
        if binary := shutil.which(name):
            engines[name] = [binary, str(Path(__file__).with_name("runner.cjs"))]
    report = {
        "warmups": args.warmups,
        "samples": args.samples,
        "binary_sha256": {name: hashlib.sha256(Path(command[0]).read_bytes()).hexdigest()
                          for name, command in engines.items()},
        "metric": "milliseconds per checked invocation; precreated inputs excluded for large diagnostics",
        "cases": {},
    }
    with tempfile.TemporaryDirectory(prefix="blitz-map-paths-") as directory:
        for name, (source, expected) in cases().items():
            path = Path(directory) / (name + ".js")
            path.write_text(source)
            measurements = {engine: [] for engine in engines}
            for order in (list(engines), list(reversed(engines))):
                for engine in order:
                    result = json.loads(run(engines[engine] + [str(path), str(args.warmups), str(args.samples)]).stdout)
                    if result["checksum"] != expected or len(result["milliseconds"]) != args.samples:
                        raise ValueError(f"invalid {engine} result for {name}")
                    result["median_ms"] = statistics.median(result["milliseconds"])
                    measurements[engine].append(result)
            report["cases"][name] = {"source": source, "expected": expected, "engines": measurements}
            print(name, " ".join(f"{engine}={statistics.median(r['median_ms'] for r in results):.3f}ms"
                                 for engine, results in measurements.items()), flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
