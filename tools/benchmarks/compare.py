#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Run identical, checked workloads serially on installed JavaScript engines."""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "crates/denko-engine/benches/fixtures"
CASES = {
    "arithmetic_multi_ops": ("sum + (prod === Infinity ? 0 : 1)", 499500),
    "property_access_chain": ("sum", 2100000),
    "array_element_access": ("sum", 150000),
    "large_array_creation": ("arr.length + arr[99999]", 199999),
    "array_map": ("squares[9] + squares.length", 110),
    "array_map_large": ("checksum", 9999900001),
    "closure_environment_creation": ("sum", 12497500),
    "string_methods": ("total", 7260000),
    "json_stringify": ("json.length", 25),
    "records_pipeline": ("checksum", 730418),
    "typed_array_sort": ("checksum", 2096128),
}


def run(command):
    return subprocess.run(command, check=True, text=True, capture_output=True, timeout=180)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--denko", type=Path, default=ROOT / "target/release/examples/engine_benchmark")
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--samples", type=int, default=11)
    parser.add_argument("--output", type=Path, default=ROOT / "target/benchmarks/engines.json")
    args = parser.parse_args()
    if args.warmups < 0 or args.samples < 1:
        parser.error("warmups must be nonnegative and samples positive")
    engines = {"denko": [str(args.denko.resolve())]}
    versions = {"rustc": run(["rustc", "--version"]).stdout.strip()}
    for engine in ("node", "bun"):
        if binary := shutil.which(engine):
            engines[engine] = [binary, str(Path(__file__).with_name("runner.cjs"))]
            versions[engine] = run([binary, "--version"]).stdout.strip()
    if "node" in engines:
        versions["v8"] = run([engines["node"][0], "-p", "process.versions.v8"]).stdout.strip()
    report = {"platform": platform.platform(), "machine": platform.machine(),
              "versions": versions, "denko_binary_sha256": hashlib.sha256(args.denko.read_bytes()).hexdigest(),
              "warmups": args.warmups, "samples": args.samples,
              "metric": "milliseconds per checked function invocation; excludes process startup and parsing",
              "cases": {}}
    with tempfile.TemporaryDirectory(prefix="blitz-bench-") as directory:
        for case_index, (name, (expression, expected)) in enumerate(CASES.items()):
            body = (FIXTURES / f"{name}.js").read_text()
            source = ("globalThis.benchmark = function () {\n" + body +
                      f'\nvar checksum = {expression};\nif (checksum !== {expected}) throw new Error("incorrect {name} result");\nreturn checksum;\n}};\n')
            path = Path(directory) / f"{name}.js"
            path.write_text(source)
            measurements = {}
            names = list(engines)
            names = names[case_index % len(names):] + names[:case_index % len(names)]
            for engine in names:
                result = json.loads(run(engines[engine] + [str(path), str(args.warmups), str(args.samples)]).stdout)
                if result["checksum"] != expected or len(result["milliseconds"]) != args.samples:
                    raise ValueError(f"invalid {engine} result for {name}")
                measurements[engine] = {**result, "median_ms": statistics.median(result["milliseconds"])}
            report["cases"][name] = {"source_sha256": hashlib.sha256(source.encode()).hexdigest(), "engines": measurements}
            print(name, " ".join(f"{engine}={value['median_ms']:.3f}ms" for engine, value in measurements.items()), flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
