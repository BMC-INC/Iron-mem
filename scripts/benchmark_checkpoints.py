#!/usr/bin/env python3
"""Measure real checkpoint CLI operations against an isolated deterministic fixture."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/debug/ironmem'))
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='ironmem-checkpoint-bench-') as temporary:
        root = Path(temporary)
        project = root/'project'
        project.mkdir()
        database = root/'state.db'
        env = dict(os.environ, DATABASE_URL=f'sqlite://{database}?mode=rwc')
        def run(*command):
            started = time.perf_counter()
            result = subprocess.run([str(args.binary.resolve()), *command], env=env, check=True, capture_output=True, text=True)
            return (time.perf_counter()-started)*1000, result.stdout
        run('snapshot', 'list')
        db = sqlite3.connect(database)
        db.execute('INSERT INTO sessions(id,project,started_at) VALUES(?,?,?)', ('fixture', str(project), 1))
        for i in range(1, 51):
            summary = f'Memory {i}: ' + 'deterministic checkpoint fixture ' * 80
            db.execute('INSERT INTO memories(rowid,project,session_id,summary,tags,created_at) VALUES(?,?,?,?,?,?)', (i,str(project),'fixture',summary,'fixture',1))
            db.execute('INSERT INTO memory_meta(memory_id,importance,created_at,evidence_root_id) VALUES(?,?,?,?)', (i,.5,1,f'fixture-root-{i}'))
            db.execute('INSERT INTO memory_evidence_roots(memory_id,evidence_root_id,role,created_at) VALUES(?,?,?,?)', (i,f'fixture-root-{i}','primary',1))
        db.commit()
        def snapshot(label, incremental=False):
            command = ['snapshot','create','--project',str(project),'--label',label]
            if incremental:
                command.append('--incremental')
            elapsed, _ = run(*command)
            row = db.execute('SELECT s.id,s.blob_hash,b.orig_len,b.comp_len FROM brain_snapshots s JOIN blobs b ON b.hash=s.blob_hash WHERE s.label=?', (label,)).fetchone()
            return {'id':row[0], 'hash':row[1], 'serialized_bytes':row[2], 'stored_bytes':row[3], 'cli_wall_ms':elapsed}
        full = snapshot('full')
        unchanged = snapshot('unchanged', True)
        if full['hash'] != unchanged['hash']:
            raise RuntimeError('No-change snapshot copied a payload')
        db.execute("UPDATE memories SET summary='One changed assertion' WHERE rowid=25")
        db.commit()
        delta = snapshot('delta', True)
        delta['restore_cli_wall_ms'], _ = run('snapshot','restore',delta['id'])
        if db.execute('SELECT summary FROM memories WHERE rowid=25').fetchone()[0] != 'One changed assertion':
            raise RuntimeError('Delta restore mismatch')
        full['restore_cli_wall_ms'], _ = run('snapshot','restore',full['id'])
        if not db.execute('SELECT summary FROM memories WHERE rowid=25').fetchone()[0].startswith('Memory 25:'):
            raise RuntimeError('Full restore mismatch')
        backup = root/'backup.json'
        run('snapshot','export',delta['id'],str(backup))
        payload = json.loads(backup.read_text())
        if payload['full'] is None or payload['parent'] is not None:
            raise RuntimeError('Export did not flatten the chain')
        result = {'schema':1, 'fixture':'50 deterministic summaries and authoritative metadata; one update; no source blobs',
                  'code_sha':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
                  'binary_sha256':hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                  'timing_scope':'CLI startup, migration and operation wall time; debug binary; no concurrent task build/test job',
                  'full':full,'unchanged':unchanged,'delta':delta,'export_bytes':backup.stat().st_size,'roundtrip_verified':True}
        (args.out/'checkpoints.json').write_text(json.dumps(result,indent=2)+'\n')
        db.close()


if __name__ == '__main__':
    main()
