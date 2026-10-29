#!/usr/bin/env python3
"""Export contracts from an already-built RuleScan executable."""
import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, default=Path("target/debug/rulescan"))
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
destination = root / "schemas"
destination.mkdir(exist_ok=True)
for kind in ("config", "output", "plan", "quarantine", "event", "commands"):
    data = subprocess.check_output([str(args.binary.resolve()), "schema", "--kind", kind])
    json.loads(data)
    filename = "commands.json" if kind == "commands" else kind + ".schema.json"
    (destination / filename).write_bytes(data)
