#!/usr/bin/env python3
"""Disposable SQLite interning experiments; never opens the live IronMem DB.

This tests one metadata family at a time with a paired 1 KiB payload per row.
It is a migration-screening microbenchmark, NOT production footprint evidence.
A migration requires >=10% total fixture savings, <=20% lookup p95 overhead,
and confirmation on an approved representative workload. Synthetic results alone
cannot enable a production schema change. NULL/empty/case are distinct values.
"""
import argparse
import hashlib
import json
import platform
import random
import sqlite3
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def run(rows=10000, seed=45):
    rng = random.Random(seed)
    payloads = [rng.randbytes(1024) for _ in range(rows)]
    families = {
        'classification': [None, '', 'internal', 'public', 'confidential', 'restricted'],
        'source_type': [None, '', 'session', 'user', 'agent', 'import'],
        'writer_identity': [None, '', 'ironmem:cli', 'ironmem:mcp:stdio', 'ironmem:rest-local'],
        'project_path': [None, ''] + [f'/Users/operator/Projects/product-{i:03d}' for i in range(30)],
        'long_project_path': [None, ''] + [f'/Users/operator/Projects/{"nested-project/" * 12}{i:03d}' for i in range(30)],
        'tags_high_cardinality': [None, '', 'Case', 'case'] + [f'topic-{i:06d}, unique-{i}' for i in range(rows)],
    }
    results = []
    with tempfile.TemporaryDirectory(prefix='ironmem-metadata-') as directory:
        for family, values in families.items():
            data = [(i + 1, values[i % len(values)], payloads[i]) for i in range(rows)]
            dbs = {}
            measures = {}
            for normalized in (False, True):
                name = 'interned' if normalized else 'inline'
                path = Path(directory) / f'{family}-{name}.db'
                db = sqlite3.connect(path)
                db.execute('PRAGMA foreign_keys=ON')
                if normalized:
                    # NULL remains a NULL reference; empty remains an interned value.
                    db.executescript('CREATE TABLE symbols(id INTEGER PRIMARY KEY,value TEXT NOT NULL UNIQUE COLLATE BINARY); CREATE TABLE records(id INTEGER PRIMARY KEY,value_id INTEGER REFERENCES symbols(id),payload BLOB NOT NULL); CREATE INDEX records_value ON records(value_id);')
                    unique = list(dict.fromkeys(v for _, v, _ in data if v is not None))
                    db.executemany('INSERT INTO symbols(id,value) VALUES(?,?)', enumerate(unique, 1))
                    ids = {v:i for i,v in enumerate(unique,1)}
                    db.executemany('INSERT INTO records VALUES(?,?,?)', [(i,ids.get(v),p) for i,v,p in data])
                    query = 'SELECT r.id,s.value,r.payload FROM records r LEFT JOIN symbols s ON s.id=r.value_id WHERE r.id BETWEEN ? AND ? ORDER BY r.id'
                else:
                    db.executescript('CREATE TABLE records(id INTEGER PRIMARY KEY,value TEXT COLLATE BINARY,payload BLOB NOT NULL); CREATE INDEX records_value ON records(value);')
                    db.executemany('INSERT INTO records VALUES(?,?,?)', data)
                    query = 'SELECT id,value,payload FROM records WHERE id BETWEEN ? AND ? ORDER BY id'
                db.commit()
                db.execute('VACUUM')
                assert db.execute(query,(1,rows)).fetchall() == data
                dbs[name]=(db,query)
                measures[name]={'database_bytes':path.stat().st_size}
            samples = {'inline':[], 'interned':[]}
            # Paired query windows; alternate order to reduce drift/cache bias.
            for iteration in range(600):
                start = rng.randint(1, max(1,rows-25))
                for name in (('inline','interned') if iteration%2 else ('interned','inline')):
                    db,query=dbs[name]
                    before=time.perf_counter_ns()
                    answer=db.execute(query,(start,start+24)).fetchall()
                    elapsed=time.perf_counter_ns()-before
                    assert len(answer)==min(25,rows-start+1)
                    if iteration>=100: samples[name].append(elapsed/1000)
            for name in samples:
                measures[name]['lookup_p50_us']=statistics.median(samples[name])
                measures[name]['lookup_p95_us']=sorted(samples[name])[int(len(samples[name])*.95)-1]
                dbs[name][0].close()
            saving=1-measures['interned']['database_bytes']/measures['inline']['database_bytes']
            overhead=measures['interned']['lookup_p95_us']/measures['inline']['lookup_p95_us']-1
            result={'family':family,'rows':rows,'unique_values_including_null':len(set(v for _,v,_ in data)), 'repeated_utf8_bytes':sum(len(v.encode()) for _,v,_ in data if v is not None)-sum(len(v.encode()) for v in set(v for _,v,_ in data) if v is not None),**measures,'fixture_storage_saving_fraction':saving,'lookup_p95_overhead_fraction':overhead,'synthetic_gate_passed':saving>=.1 and overhead<=.2,'migration_enabled':False}
            results.append(result)
    return {'schema_version':1,'seed':seed,'rows':rows,'python':platform.python_version(),'sqlite':sqlite3.sqlite_version,'platform':platform.platform(),'cache_condition':'warm process; OS cache uncontrolled','workload':'synthetic single-family projections with 1024-byte incompressible payload, not a full IronMem database','gates':{'minimum_fixture_saving_fraction':.1,'maximum_lookup_p95_overhead_fraction':.2,'representative_workload_required':True},'results':results}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--rows',type=int,default=10000)
    args=parser.parse_args()
    if args.rows<100: parser.error('--rows must be >=100')
    result=run(args.rows)
    result['code_sha']=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    result['script_sha256']=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    args.out.parent.mkdir(parents=True,exist_ok=True)
    args.out.write_text(json.dumps(result,indent=2)+'\n')
    for row in result['results']:
        print(f"{row['family']}: storage {row['fixture_storage_saving_fraction']:+.1%}, lookup p95 overhead {row['lookup_p95_overhead_fraction']:+.1%}, synthetic gate {row['synthetic_gate_passed']}")


if __name__=='__main__': main()
