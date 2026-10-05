#!/usr/bin/env python3
"""Reproduce the containment evidence and time it, for pasting into the validation log.

Runs the escape suite (`crates/bollo-workspace/tests/container_escapes.rs`) and the
contained link proof (`crates/bollo-workspace/tests/container_toolchain.rs`) on the
host toolchain and prints a Markdown block with the per-phase and per-test wall
clocks. Every test is invoked through its own `cargo test`, so the printed block
describes exactly the gate CI runs; the per-test numbers come from libtest itself.

    python scripts/containment_evidence.py

Exit codes: 0 every test passed, 1 the build or a test failed, 2 nothing ran (the
targets are Windows-only, so a non-Windows host has no such tests).
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import socket
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOGS = ROOT / 'target' / 'containment-evidence'
PACKAGE = 'bollo-workspace'
# What the validation log records as evidence: the escape suite and the link proof.
PHASES = (
    ('escape suite', 'container_escapes'),
    ('contained link proof', 'container_toolchain'),
)
RESULT = re.compile(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored')
FINISHED = re.compile(r'finished in ([\d.]+)s')
LISTED = re.compile(r'^(.+): test$')


@dataclass
class Test:
    """One test as executed by its own `cargo test`, with both clocks."""

    phase: str
    target: str
    name: str
    result: str = 'not run'
    test_seconds: float = 0.0
    wall_seconds: float = 0.0
    log: Path = field(default_factory=Path)

    @property
    def passed(self) -> bool:
        return self.result == 'ok'


def cargo() -> str:
    found = shutil.which('cargo')
    if found is None:
        sys.exit('cargo is not on PATH; install the pinned toolchain first')
    return found


def invoke(binary: str, args: list[str], log: Path) -> tuple[int, bytes, float]:
    """Run a command from the repository root, capture it, and time the wall clock."""
    env = dict(os.environ, CARGO_TERM_COLOR='never', NO_COLOR='1')
    started = time.perf_counter()
    completed = subprocess.run(
        [binary, *args], cwd=ROOT, env=env, capture_output=True
    )
    elapsed = time.perf_counter() - started
    payload = completed.stdout + completed.stderr
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_bytes(payload)
    return completed.returncode, payload, elapsed


def discover(binary: str, target: str) -> list[str]:
    """The test names in one target, read from libtest rather than hard-coded."""
    _, payload, _ = invoke(
        binary,
        ['test', '-p', PACKAGE, '--test', target, '--locked', '--', '--list'],
        LOGS / f'{target}--list.log',
    )
    names = [match.group(1) for line in payload.decode('utf-8', 'replace').splitlines()
             if (match := LISTED.match(line.strip()))]
    return names


def one_test(binary: str, phase: str, target: str, name: str) -> Test:
    test = Test(phase=phase, target=target, name=name)
    test.log = LOGS / f'{target}--{name.replace("::", "_")}.log'
    code, payload, wall = invoke(
        binary,
        ['test', '-p', PACKAGE, '--test', target, '--locked',
         '--', name, '--exact', '--test-threads=1'],
        test.log,
    )
    text = payload.decode('utf-8', 'replace')
    test.wall_seconds = wall
    summary = RESULT.search(text)
    finished = FINISHED.search(text)
    if summary is not None:
        if finished is not None:
            test.test_seconds = float(finished.group(1))
        test.result = 'ok' if code == 0 and summary.group(3) == '0' else 'failed'
    else:
        test.result = 'failed'
    return test


def revision() -> str:
    if shutil.which('git') is None:
        return 'unknown'
    completed = subprocess.run(
        ['git', 'rev-parse', '--short', 'HEAD'], cwd=ROOT, capture_output=True, text=True
    )
    return completed.stdout.strip() or 'unknown'


def toolchain(binary: str) -> str:
    """"rustc 1.93.1 (...) / host x86_64-pc-windows-msvc", straight from rustc."""
    rustc = Path(binary).with_name('rustc.exe' if os.name == 'nt' else 'rustc')
    if not rustc.is_file():
        rustc = Path(shutil.which('rustc') or binary)
    completed = subprocess.run([str(rustc), '-vV'], capture_output=True, text=True)
    if completed.returncode != 0:
        return 'unknown'
    lines = completed.stdout.splitlines()
    host = next((line for line in lines if line.startswith('host:')), 'host: unknown')
    return f'{lines[0]} / {host}' if lines else 'unknown'


def seconds(value: float) -> str:
    return f'{value:.1f} s'


def stamp() -> str:
    return time.strftime('%Y-%m-%d %H:%M:%SZ', time.gmtime())


def environment(with_host: bool) -> str:
    """`ci` or `local`, never a machine name unless one is asked for: this block gets
    pasted into a tracked document, and the distinction that matters is only whether
    the numbers came from a hosted runner or from a workstation."""
    where = 'ci' if os.environ.get('GITHUB_ACTIONS') or os.environ.get('CI') else 'local'
    return f'{where} ({socket.gethostname()})' if with_host else where


def report(binary: str, where: str, build: tuple[str, float] | None, tests: list[Test],
           tails: dict[Path, str]) -> str:
    """The paste-ready block: phases first, then one row per test."""
    passed = sum(test.passed for test in tests)
    failed = len(tests) - passed
    ok = failed == 0 and tests
    lines = [
        f'### Containment evidence — {stamp()} — {where} — `{revision()}`',
        '',
        'One command, from the repository root:',
        '',
        '```sh',
        'python scripts/containment_evidence.py',
        '```',
        '',
        f'Toolchain: `{toolchain(binary)}`. Each test runs through its own `cargo test`,',
        'single-threaded, so the phase numbers are the gate CI runs and the per-test',
        'numbers are the times libtest reports for the test itself.',
        '',
        '| Phase | Tests | Result | Wall clock |',
        '| --- | --- | --- | --- |',
    ]
    if build is not None:
        state, elapsed = build
        lines.append(f'| Build test binaries | — | {state} | {seconds(elapsed)} |')
    for phase, target in PHASES:
        members = [test for test in tests if test.phase == phase]
        if not members:
            lines.append(f'| {phase} (`{target}`) | 0 | not run | — |')
            continue
        won = sum(test.passed for test in members)
        state = 'ok' if won == len(members) else 'failed'
        wall = sum(test.wall_seconds for test in members)
        lines.append(
            f'| {phase} (`{target}`) | {won}/{len(members)} passed | {state} | {seconds(wall)} |'
        )
    build_wall = build[1] if build is not None else 0.0
    total_wall = seconds(build_wall + sum(test.wall_seconds for test in tests))
    if not tests:
        lines.append('| **Total** | **no tests discovered** | **not run** | — |')
    else:
        state = 'ok' if ok else 'failed'
        lines.append(
            f'| **Total** | **{passed}/{len(tests)} passed** | **{state}** | **{total_wall}** |'
        )
    if tests:
        lines += [
            '',
            'Per test (own libtest time, then the enclosing `cargo test` wall clock):',
            '',
            '| Test | Result | Test time | cargo wall |',
            '| --- | --- | --- | --- |',
        ]
        for test in tests:
            lines.append(
                f'| `{test.target}::{test.name}` | {test.result} | '
                f'{seconds(test.test_seconds)} | {seconds(test.wall_seconds)} |'
            )
    else:
        lines += [
            '',
            'No tests were discovered: both targets are `#![cfg(windows)]`, so this host',
            'cannot produce containment evidence.',
        ]
    if failed:
        lines += ['', 'Failing output (full logs stay under `target/containment-evidence/`):', '']
        for test in tests:
            if test.passed:
                continue
            body = tails.get(test.log, '').rstrip('\n')
            lines += [f'`{test.target}::{test.name}` — `{test.log.relative_to(ROOT)}`', '',
                      '```text', body, '```', '']
    return '\n'.join(lines).rstrip() + '\n'


def tail(path: Path, lines: int) -> str:
    text = path.read_text(encoding='utf-8', errors='replace').rstrip('\n')
    return '\n'.join(text.splitlines()[-lines:])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tail', type=int, default=40,
                        help='lines of failing output to print (default: 40)')
    parser.add_argument('--host', action='store_true',
                        help='name this machine in the heading (default: ci/local only)')
    args = parser.parse_args()

    if hasattr(sys.stdout, 'reconfigure'):
        sys.stdout.reconfigure(encoding='utf-8')

    binary = cargo()
    targets = [target for _, target in PHASES]
    build_args = ['test', '-p', PACKAGE]
    for target in targets:
        build_args += ['--test', target]
    build_args += ['--no-run', '--locked']
    code, payload, elapsed = invoke(binary, build_args, LOGS / 'build.log')
    build = ('ok', elapsed) if code == 0 else ('failed', elapsed)

    tests: list[Test] = []
    if code == 0:
        for phase, target in PHASES:
            for name in discover(binary, target):
                tests.append(one_test(binary, phase, target, name))

    tails = {test.log: tail(test.log, args.tail) for test in tests if not test.passed}
    block = report(binary, environment(args.host), build, tests, tails)
    # Written as well as printed, so the block survives a console that mangles UTF-8.
    evidence = LOGS / 'evidence.md'
    evidence.write_text(block, encoding='utf-8', newline='\n')
    print(block, end='')
    print(f'block also written to {evidence.relative_to(ROOT)}', file=sys.stderr)
    if code != 0:
        print(f'build failed; see {LOGS.relative_to(ROOT) / "build.log"}:', file=sys.stderr)
        print('\n'.join(payload.decode('utf-8', 'replace').splitlines()[-args.tail:]),
              file=sys.stderr)
        return 1
    if not tests:
        return 2
    return 0 if all(test.passed for test in tests) else 1


if __name__ == '__main__':
    sys.exit(main())
