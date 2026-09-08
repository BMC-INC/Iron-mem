#!/usr/bin/env python3
"""Offline native SQLite retention inventory and explicit whole-store physical purge.

Selective forget belongs to IronMem's governed CLI. This tool never rewrites a
subset of immutable chains. It does not erase filesystem snapshots or SSD cells.
"""
from contextlib import closing
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3


def quote(name):
    return '"' + name.replace('"', '""') + '"'


def open_existing(path):
    path = Path(path)
    if path.is_symlink(): raise ValueError('refusing symlink database')
    return sqlite3.connect(path.resolve().as_uri()+'?mode=rw',uri=True,timeout=0)


def inventory(connection, path):
    schemas=connection.execute("SELECT name,type,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name").fetchall()
    tables=[(name,sql) for name,kind,sql in schemas if kind=='table' and not name.startswith('sqlite_')]
    if not any(name=='memory_meta' for name,_ in tables): raise ValueError('not an initialized IronMem database')
    unsupported=[name for name,sql in tables if 'CREATE VIRTUAL TABLE' in sql.upper() and 'USING FTS5' not in sql.upper()]
    if unsupported: raise ValueError('offline Python purge cannot load virtual modules: '+', '.join(unsupported)+'; use a module-aware database administrator')
    hash_state=hashlib.sha256()
    hash_state.update(str(Path(path).resolve()).encode())
    hash_state.update(json.dumps(schemas).encode())
    counts={}
    for name,sql in tables:
        # Virtual FTS state is included through its ordinary shadow tables.
        if 'CREATE VIRTUAL TABLE' in sql.upper():
            counts[name]=connection.execute(f'SELECT COUNT(*) FROM {quote(name)}').fetchone()[0]
            continue
        fields=[r[1] for r in connection.execute(f'PRAGMA table_info({quote(name)})')]
        order=','.join(quote(n) for n in fields)
        count=0
        hash_state.update(name.encode())
        for row in connection.execute(f'SELECT * FROM {quote(name)} ORDER BY {order}'):
            # Exact typed values, never included in the report.
            encoded=json.dumps([{'blob':v.hex()} if isinstance(v,bytes) else v for v in row],ensure_ascii=True,separators=(',',':')).encode()
            hash_state.update(len(encoded).to_bytes(8,'big')); hash_state.update(encoded); count+=1
        counts[name]=count
    holds=connection.execute('SELECT COUNT(*) FROM memory_meta WHERE legal_hold<>0').fetchone()[0]
    return {'schema':1,'database':str(Path(path).resolve()),'plan_sha256':hash_state.hexdigest(),
            'tables':counts,'layers':{name:counts.get(table,0) for name,table in {'stored_memory_rows':'memories','retained_metadata':'memory_meta','immutable_assertions':'assertion_events','audit_entries':'memory_ledger','source_objects':'blobs','snapshots':'brain_snapshots','sessions':'sessions','observations':'observations'}.items()},'current_legal_holds':holds,'scope':'all namespaces and all history in this SQLite store',
            'retained_outside_store':['exported checkpoints','database backups','filesystem/cloud snapshots',
                                      'generated IRONMEM.md and user-edited context','external vector/graph services','client transcripts/logs'],
            'physical_erasure_guarantee':False,
            'meaning':'Purge removes all SQLite tables, then vacuums and truncates the journal. It cannot guarantee erasure of storage-device or external copies.'}


def plan(path):
    with closing(open_existing(path)) as connection:
        connection.execute('BEGIN')
        result=inventory(connection,path)
        connection.rollback()
        return result


def purge(path, expected, offline, erase_history):
    if not offline or not erase_history: raise ValueError('purge requires --offline and --erase-all-history after stopping every client and service')
    connection=open_existing(path)
    try:
        # Switching away from WAL and holding an exclusive lock rejects active
        # readers/writers. Idle clients must still be stopped by the operator.
        connection.execute('PRAGMA busy_timeout=0')
        connection.execute('PRAGMA foreign_keys=OFF')
        connection.execute('PRAGMA locking_mode=EXCLUSIVE')
        if connection.execute('PRAGMA journal_mode=DELETE').fetchone()[0].lower()!='delete': raise ValueError('cannot obtain offline journal mode')
        connection.execute('BEGIN EXCLUSIVE')
        result=inventory(connection,path)
        if result['plan_sha256']!=expected: raise ValueError('store changed; generate and review a new plan')
        if result['current_legal_holds']: raise ValueError('legal holds prohibit purge; use the governed hold workflow first')
        connection.execute('PRAGMA secure_delete=ON')
        for name, in connection.execute("SELECT name FROM sqlite_master WHERE type='trigger'").fetchall():
            connection.execute(f'DROP TRIGGER {quote(name)}')
        virtual=connection.execute("SELECT name FROM sqlite_master WHERE type='table' AND upper(sql) LIKE '%CREATE VIRTUAL TABLE%'").fetchall()
        for name, in virtual: connection.execute(f'DROP TABLE {quote(name)}')
        for name, in connection.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetchall():
            connection.execute(f'DROP TABLE {quote(name)}')
        connection.commit()
        try:
            connection.execute('VACUUM')
            result['compaction']='complete'
        except sqlite3.Error as error:
            result['compaction']='failed: '+str(error)
        result['purged']=True
        result['remaining_application_tables']=connection.execute("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetchone()[0]
        result['next_step']='Reinitialize with IronMem only after reviewing external copies; do not restore a backup if erasure must persist.'
        return result
    except Exception:
        connection.rollback()
        raise
    finally:
        connection.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation',choices=['plan','purge-store'])
    parser.add_argument('--database',type=Path,required=True)
    parser.add_argument('--confirm-plan')
    parser.add_argument('--offline',action='store_true')
    parser.add_argument('--erase-all-history',action='store_true')
    args=parser.parse_args()
    result=plan(args.database) if args.operation=='plan' else purge(args.database,args.confirm_plan,args.offline,args.erase_all_history)
    print(json.dumps(result,indent=2,sort_keys=True))
    if result.get("purged") and result.get("compaction") != "complete": raise SystemExit(2)


if __name__=='__main__': main()
