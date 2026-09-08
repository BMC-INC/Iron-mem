#!/usr/bin/env python3
"""Real isolated SQLite failures and bounded release-build scale/recovery measurements."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sqlite3
from evidence_common import connection
import statistics
import subprocess
import sys
import tempfile
import time
from evidence_common import Fixture, clean_env, digest, provenance, write_report


def seed(fixture,n):
    fixture.run('snapshot','list')
    with connection(fixture.database) as db:
        db.execute('INSERT INTO sessions(id,project,started_at) VALUES(?,?,?)',('fixture',str(fixture.project),1))
        for i in range(1,n+1):
            db.execute('INSERT INTO memories(rowid,project,session_id,summary,tags,created_at) VALUES(?,?,?,?,?,?)',
                       (i,str(fixture.project),'fixture',f'Architecture fixture {i}: durable recovery sqlite '+('payload '*128),'architecture recovery sqlite',1))
            db.execute('INSERT INTO memory_meta(memory_id,importance,created_at,evidence_root_id) VALUES(?,?,?,?)',(i,.5,1,f'fixture-{i}'))
            db.execute('INSERT INTO memory_evidence_roots(memory_id,evidence_root_id,role,created_at) VALUES(?,?,?,?)',(i,f'fixture-{i}','primary',1))


def snapshot(fixture,label,incremental=False):
    command=['snapshot','create','--project',str(fixture.project),'--label',label]
    if incremental: command.append('--incremental')
    _,ms=fixture.run(*command)
    with connection(fixture.database) as db:
        row=db.execute('SELECT id,blob_hash FROM brain_snapshots WHERE label=?',(label,)).fetchone()
    return row[0],row[1],ms


def faults(fixture,cycles):
    seed(fixture,20)
    outcomes={}
    # The writer dies after SQLite accepts a write but before COMMIT.
    code="import sqlite3,os,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); c.execute(\"INSERT INTO sessions(id,project,started_at) VALUES('crash','fixture',1)\"); os._exit(23)"
    child=subprocess.run([sys.executable,'-c',code,str(fixture.database)],env=clean_env(),timeout=30)
    with connection(fixture.database) as db:
        assert child.returncode==23 and db.execute("SELECT COUNT(*) FROM sessions WHERE id='crash'").fetchone()[0]==0
        assert db.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
        outcomes['process_exit_before_commit']='rollback verified'
        pages=db.execute('PRAGMA page_count').fetchone()[0]
        db.execute(f'PRAGMA max_page_count={pages}')
        try:
            db.execute("INSERT INTO blobs(hash,content_type,codec,orig_len,comp_len,data,created_at) VALUES('disk-full','fixture','raw',1048576,1048576,zeroblob(1048576),1)")
            raise AssertionError('page exhaustion did not fire')
        except sqlite3.OperationalError as error:
            assert 'full' in str(error).lower(),str(error)
            db.rollback()
        assert db.execute("SELECT COUNT(*) FROM blobs WHERE hash='disk-full'").fetchone()[0]==0
        assert db.execute('SELECT COUNT(*) FROM memories').fetchone()[0]==20
        outcomes['sqlite_page_exhaustion']='SQLITE_FULL rollback verified; not a host filesystem exhaustion test'
        db.execute('PRAGMA max_page_count=1073741823')
    def writer(i): return fixture.run('remember',f'concurrent fixture {i}','--project',str(fixture.project))[0]
    with ThreadPoolExecutor(max_workers=4) as pool: list(pool.map(writer,range(12)))
    with connection(fixture.database) as db:
        assert db.execute('SELECT COUNT(*) FROM memories').fetchone()[0]==32
        assert db.execute('SELECT COUNT(DISTINCT rowid) FROM memories').fetchone()[0]==32
    outcomes['concurrent_cli_writers']={'writers':4,'committed_memories':12,'unique_ids':True}
    checkpoint,hash_value,_=snapshot(fixture,'corruption')
    with connection(fixture.database) as db:
        original=db.execute('SELECT data FROM blobs WHERE hash=?',(hash_value,)).fetchone()[0]
        db.execute('UPDATE blobs SET data=? WHERE hash=?',(b'corrupt-fixture',hash_value))
    try:
        fixture.run('snapshot','restore',checkpoint)
        raise AssertionError('corrupted checkpoint accepted')
    except RuntimeError:
        pass
    with connection(fixture.database) as db:
        assert db.execute('SELECT COUNT(*) FROM memories').fetchone()[0]==32
        db.execute('UPDATE blobs SET data=? WHERE hash=?',(original,hash_value))
    outcomes['corrupted_checkpoint']='refused before destructive restore; original memory count preserved'
    fixture.run('snapshot','delete',checkpoint)
    started=time.perf_counter()
    for cycle in range(cycles):
        full,_,_=snapshot(fixture,f'full-{cycle}')
        with connection(fixture.database) as db: db.execute('UPDATE memories SET summary=? WHERE rowid=1',(f'recovery cycle {cycle}',))
        delta,_,_=snapshot(fixture,f'delta-{cycle}',True)
        fixture.run('snapshot','restore',full)
        fixture.run('snapshot','restore',delta)
        with connection(fixture.database) as db:
            assert db.execute('SELECT summary FROM memories WHERE rowid=1').fetchone()[0]==f'recovery cycle {cycle}'
            assert db.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
        fixture.run('snapshot','delete',delta); fixture.run('snapshot','delete',full)
    outcomes['repeated_full_delta_recovery']={'cycles':cycles,'wall_seconds':time.perf_counter()-started}
    return outcomes


def disk_full(binary, mount):
    import errno
    import shutil
    mount=Path(mount).resolve()
    if mount.stat().st_dev == mount.parent.stat().st_dev or shutil.disk_usage(mount).total > 128*1024*1024:
        raise ValueError('disk-full fixture requires a separate disposable filesystem of at most 128 MiB')
    fixture=Fixture(mount/'fixture',binary)
    seed(fixture,1)
    filler=mount/'filler'
    if filler.exists(): raise ValueError('refusing existing filler')
    filled=0
    try:
        with filler.open('xb',buffering=0) as stream:
            try:
                while filled < 128*1024*1024:
                    filled += stream.write(b'x'*65536)
                raise AssertionError('filesystem did not fill within bound')
            except OSError as error:
                if error.errno != errno.ENOSPC: raise
        try:
            fixture.run('remember','disk full canary '+('x'*16000),'--project',str(fixture.project))
            raise AssertionError('write unexpectedly succeeded on full filesystem')
        except RuntimeError as error:
            if not any(word in str(error).lower() for word in ('full','space','disk i/o')): raise
    finally:
        filler.unlink(missing_ok=True)
    with connection(fixture.database) as db:
        assert db.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
        assert db.execute('SELECT COUNT(*) FROM memories').fetchone()[0]==1
    fixture.run('remember','write after space reclaimed','--project',str(fixture.project))
    with connection(fixture.database) as db:
        assert db.execute('SELECT COUNT(*) FROM memories').fetchone()[0]==2
    return {'enospc_observed':True,'filler_bytes':filled,'failed_write_preserved_memory':True,'write_after_reclaim':True}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,default=Path('target/release/ironmem'))
    p.add_argument('--disk-full-root',type=Path,help='separate disposable mounted filesystem, <=128 MiB; never a normal folder')
    p.add_argument('--sizes',default='100,1000,10000')
    p.add_argument('--samples',type=int,default=10)
    p.add_argument('--cycles',type=int,default=20)
    p.add_argument('--build-profile',choices=['release','debug','unknown'],default='unknown')
    p.add_argument('--out',type=Path,required=True)
    args=p.parse_args()
    sizes=list(map(int,args.sizes.split(',')))
    if any(n<1 or n>1000000 for n in sizes) or not 1<=args.cycles<=10000 or not 2<=args.samples<=1000:
        p.error('invalid sizes, cycles or samples')
    report={'schema':1,'provenance':provenance(args.binary),'build_profile_operator_declared':args.build_profile,
            'runner_sha256':digest(Path(__file__).read_bytes()),'scale':[],
            'limitations':['Synthetic ~1KB summaries; no model or external backend calls.',
                          'CLI startup/migration/diagnostics included; first and warm samples reported separately.',
                          'SQLite page exhaustion is not physical disk exhaustion. Bounded cycles are not months of production evidence.']}
    with tempfile.TemporaryDirectory(prefix='ironmem-reliability-') as temporary:
        root=Path(temporary)
        report['faults']=faults(Fixture(root/'faults',args.binary),args.cycles)
        for n in sizes:
            fixture=Fixture(root/f'scale-{n}',args.binary); seed(fixture,n)
            _,first=fixture.diagnose('architecture sqlite',2000,False)
            samples=[fixture.diagnose('architecture sqlite',2000,False)[1] for _ in range(args.samples)]
            _,_,checkpoint_ms=snapshot(fixture,'scale')
            storage=sum(p.stat().st_size for p in fixture.root.glob('state.db*'))
            report['scale'].append({'memories':n,'first_cli_ms':first,'warm_cli_ms':samples,'p50_ms':statistics.median(samples),
                                    'p95_ms':sorted(samples)[min(len(samples)-1,int(.95*len(samples)))],
                                    'checkpoint_cli_ms':checkpoint_ms,'db_and_wal_bytes':storage})
    report['physical_disk_full']=disk_full(args.binary,args.disk_full_root) if args.disk_full_root else {'tested':False}
    write_report(args.out,report)


if __name__=='__main__': main()
