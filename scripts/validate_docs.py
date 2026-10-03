#!/usr/bin/env python3
"""Validate documentation contracts, not an unimplemented Bollo runtime.

No network access, provider calls or upstream checkout required.
Install requirements-docs.txt into a virtualenv before running.
"""
from __future__ import annotations

import copy
import csv
import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

from jsonschema import Draft202012Validator, FormatChecker
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / 'docs'
CHECKS = 0


def check(condition: bool, message: str) -> None:
    global CHECKS
    CHECKS += 1
    if not condition:
        raise AssertionError(message)


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'Duplicate JSON key: {key}')
        result[key] = value
    return result


def load(path: Path):
    return json.loads(path.read_text(), object_pairs_hook=no_duplicates)


def validate(schema, value):
    Draft202012Validator(schema, format_checker=FormatChecker()).validate(value)


def rejects(schema, value, label):
    errors = list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(value))
    check(bool(errors), f'Negative fixture unexpectedly accepted: {label}')


def main():
    # All JSON parses strictly, then independent schemas are metaschema-checked.
    json_files = list(DOCS.rglob('*.json'))
    for file in json_files:
        load(file)
    schemas = {}
    for file in (DOCS / 'contracts').glob('*.schema.json'):
        schema = load(file)
        Draft202012Validator.check_schema(schema)
        schemas[file.name] = schema
    api = load(DOCS / 'contracts/openapi.json')
    validate_openapi(api)
    check(api['security'] == [{'bearerAuth': []}], 'All API operations must inherit bearer auth')
    operation_ids = []
    for path, methods in api['paths'].items():
        for method, operation in methods.items():
            operation_ids.append(operation['operationId'])
            check(operation.get('security', api['security']) != [], f'Unauthenticated route: {path}')
            check(operation['x-phase'] == 'P2', f'Unexpected API phase: {path}')
            check(operation['x-required-capability'] in ('read', 'run', 'approve'), f'Missing scope: {path}')
            check(f'{method.upper()} {path}' in (DOCS / 'reference/api.md').read_text(), f'Undocumented route: {path}')
    check(len(operation_ids) == len(set(operation_ids)), 'Duplicate operationId')

    for filename in ['config.balanced.json', 'config.workspace-auto.json']:
        validate(schemas['config.schema.json'], load(DOCS / 'examples' / filename))
    validate(schemas['tools.schema.json'], load(DOCS / 'examples/tool.apply-patch.json'))
    validate(schemas['approval.schema.json'], load(DOCS / 'examples/approval.json'))
    policy_cases = load(DOCS / 'examples/policy-cases.json')
    validate(schemas['policy-cases.schema.json'], policy_cases)
    check(len({c['id'] for c in policy_cases['cases']}) == len(policy_cases['cases']), 'Duplicate policy-case IDs')

    events = []
    for line in (DOCS / 'examples/session.ndjson').read_text().splitlines():
        event = json.loads(line, object_pairs_hook=no_duplicates)
        validate(schemas['event.schema.json'], event)
        events.append(event)
    check([e['seq'] for e in events] == list(range(1, len(events) + 1)), 'Fixture sequence is not continuous')
    check(len({e['event_id'] for e in events}) == len(events), 'Repeated fixture event ID')
    check(events[0]['type'] == 'run.started' and events[-1]['type'] == 'run.finished', 'Fixture lifecycle boundary')
    api_event = copy.deepcopy(api['components']['schemas']['Event'])
    independent_event = {k: v for k, v in schemas['event.schema.json'].items() if k != '$schema'}
    check(api_event == independent_event, 'OpenAPI Event drifted from standalone event schema')
    check(api['components']['schemas']['ApprovalDecision'] == {k: v for k, v in schemas['approval.schema.json'].items() if k != '$schema'}, 'Approval schema drift')
    for name, example in load(DOCS / 'examples/api.json').items():
        validate(api['components']['schemas'][name], example)
    sample_policy = load(DOCS / 'examples/api.json')['Policy']
    check({r['id'] for r in sample_policy['rules']} == {p['rule_id'] for p in sample_policy['provenance']}, 'Policy fixture lacks provenance')

    # Mutations ensure key schema constraints actually reject bad inputs.
    base = load(DOCS / 'examples/config.workspace-auto.json')
    bad = copy.deepcopy(base); bad['permissions']['sandbox'] = 'off'
    rejects(schemas['config.schema.json'], bad, 'workspace_auto without workspace sandbox')
    bad = copy.deepcopy(base); bad['provider']['api_key'] = 'secret-must-not-be-a-config-field'
    rejects(schemas['config.schema.json'], bad, 'literal secret field')
    bad = copy.deepcopy(base); bad['permissions']['profile'] = 'yolo'
    rejects(schemas['config.schema.json'], bad, 'unknown preset')
    bad = copy.deepcopy(events[0]); bad['data'] = {'state': 'completed'}
    rejects(schemas['event.schema.json'], bad, 'wrong typed event payload')
    bad = copy.deepcopy(events[0]); bad['run_id'] = None
    rejects(schemas['event.schema.json'], bad, 'missing run identity')
    bad = copy.deepcopy(events[-2]); bad['data']['cost_microusd'] = 0
    rejects(schemas['event.schema.json'], bad, 'unknown cost represented as free')
    bad = copy.deepcopy(events[0]); bad['timestamp'] = 'not-a-date'
    rejects(schemas['event.schema.json'], bad, 'invalid RFC3339 timestamp')
    rejects(schemas['tools.schema.json'], {'name': 'disable_policy', 'arguments': {}}, 'invented privileged tool')
    bad = load(DOCS / 'examples/tool.apply-patch.json'); del bad['arguments']['expected_sha256']
    rejects(schemas['tools.schema.json'], bad, 'patch without preimage guard')
    rejects(api['components']['schemas']['CreateSession'], {'workspace': '/etc'}, 'API caller-selected arbitrary root')
    bad = load(DOCS / 'examples/approval.json'); bad['intent_hash'] = 'not-a-hash'
    rejects(schemas['approval.schema.json'], bad, 'invalid approval hash')

    # Requirement/document/test mapping, not runtime acceptance execution.
    registry = load(DOCS / 'product/requirements.json')['requirements']
    check([r['id'] for r in registry] == [f'BH-{i:03}' for i in range(1, 21)], 'Requirement IDs missing/reordered')
    check(len({r['acceptance_test'] for r in registry}) == len(registry), 'Duplicate acceptance IDs')
    trace = (DOCS / 'product/traceability.md').read_text()
    tests = (DOCS / 'delivery/testing.md').read_text()
    for requirement in registry:
        check((DOCS / requirement['spec']).is_file(), f'Missing spec: {requirement}')
        check(requirement['id'] in trace and requirement['acceptance_test'] in trace, f'Traceability missing: {requirement}')
        check(f"### {requirement['acceptance_test']} —" in tests, f'Missing test definition: {requirement}')
        check(requirement['acceptance'] in tests, f'Acceptance wording drift: {requirement}')
        check(requirement['implementation_status'] == 'not_implemented', 'Docs-only baseline overclaims implementation')
    check(sum(r['phase'] == 'MVP' for r in registry) == 16, 'MVP scope drift')
    known_requirements = {r['id'] for r in registry}
    known_tests = {r['acceptance_test'] for r in registry}

    markdown = [ROOT / 'README.md', *DOCS.rglob('*.md')]
    atlas = (DOCS / 'README.md').read_text()
    diagrams = 0
    for file in markdown:
        text = file.read_text()
        check(len(re.findall(r'^```', text, re.M)) % 2 == 0, f'Unbalanced fenced blocks: {file}')
        diagrams += len(re.findall(r'^```mermaid\s*$', text, re.M))
        for match in re.finditer(r'\b(?:BH|AT)-\d{3}\b', text):
            token = match.group()
            check(token in (known_requirements if token.startswith('BH') else known_tests), f'Unknown trace ID {token}: {file}')
        if file.parent != ROOT and file.name != 'README.md':
            check(file.relative_to(DOCS).as_posix() in atlas, f'Missing atlas entry: {file}')
        # Inline Markdown links; exclude fenced code examples and all external schemes.
        without_code = re.sub(r'```.*?```', '', text, flags=re.S)
        for target in re.findall(r'(?<!!)\[[^\]]*\]\(([^\s)]+)(?:\s+"[^"]*")?\)', without_code):
            parsed = urlsplit(target.strip('<>'))
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            path = (file.parent / unquote(parsed.path)).resolve()
            check(path.is_relative_to(ROOT), f'Link escapes repo: {file} -> {target}')
            check(path.exists(), f'Broken link: {file.relative_to(ROOT)} -> {target}')
    check(diagrams >= 8, 'Missing visual coverage')

    lock = load(DOCS / 'research/upstream-lock.json')
    inventory_rows = 0
    for repo in lock['repositories']:
        check(bool(re.fullmatch('[0-9a-f]{40}', repo['commit'])), 'Invalid pinned upstream commit')
        with (DOCS / 'research' / repo['inventory']).open(newline='') as f:
            rows = list(csv.DictReader(f))
        check(len(rows) == repo['tracked_files'], 'Source inventory count mismatch')
        check(len({r['path'] for r in rows}) == len(rows), 'Duplicate source inventory path')
        for row in rows:
            check(bool(re.fullmatch('[0-9a-f]{64}', row['sha256'])), 'Invalid inventory hash')
            check(f"/blob/{repo['commit']}/" in row['permalink'], 'Unpinned inventory URL')
            check(int(row['bytes']) >= 0, 'Invalid file byte count')
        inventory_rows += len(rows)
    print(f'PASS: {len(markdown)} Markdown documents; {len(schemas)} JSON Schemas; '
          f'{len(operation_ids)} OpenAPI operations; {len(registry)} requirements; '
          f'{len(events)} NDJSON events; {len(policy_cases["cases"])} policy fixtures; '
          f'{inventory_rows} upstream paths; {diagrams} Mermaid blocks; {CHECKS} assertions.')
    print('Scope: contracts/fixtures/links/traceability only. No runtime tests, external link checks, or Mermaid rendering executed here.')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'FAIL: {error}', file=sys.stderr)
        sys.exit(1)
