"""Isolated public-fixture CLI operations; never load the operator's settings."""
from contextlib import contextmanager
import sqlite3
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import time


def digest(data):
    return hashlib.sha256(data).hexdigest()


def clean_env():
    # A deliberately small environment: do not inherit cloud keys or DB overrides.
    names = ('PATH', 'SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TMPDIR', 'TEMP', 'TMP')
    return {key: os.environ[key] for key in names if key in os.environ}


class Fixture:
    def __init__(self, root, binary):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        self.project = self.root / 'project'
        self.project.mkdir(exist_ok=True)
        self.database = self.root / 'state.db'
        self.config = self.root / 'settings.json'
        self.config.write_text(json.dumps({'db_path': str(self.database), 'assertions': {'enabled': True}}))
        self.binary = str(Path(binary).resolve())

    def run(self, *args, timeout=120):
        started = time.perf_counter()
        result = subprocess.run([self.binary, '--config', str(self.config), *map(str, args)],
                                env=clean_env(), cwd=self.project, capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f'{args[0]} failed: {result.stderr[-3000:]}')
        return result.stdout, (time.perf_counter() - started) * 1000

    def diagnose(self, query, budget=2000, content=True):
        out, elapsed = self.run('diagnose', json.dumps({'namespace': 'local', 'project': str(self.project),
                                                     'query': query, 'include_content': content, 'budget_bytes': budget}))
        return json.loads(out), elapsed


def provenance(binary):
    return {'binary_sha256': digest(Path(binary).read_bytes()), 'platform': platform.platform(),
            'machine': platform.machine(), 'processor': platform.processor(), 'python': platform.python_version(),
            'cpu_count': os.cpu_count(), 'timing_clock': 'perf_counter',
            'machine_load': 'uncontrolled; arms run serially on the same host'}


def write_report(path, report):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    # Do not silently relabel or overwrite prior measurements.
    with path.open('x', encoding='utf-8') as stream:
        json.dump(report, stream, indent=2, sort_keys=True)
        stream.write('\n')

@contextmanager
def connection(*args, **kwargs):
    db = sqlite3.connect(*args, **kwargs)
    try:
        with db:
            yield db
    finally:
        db.close()
