#!/usr/bin/env python3
"""Focused integration checks for isolated install, retention, and MCP diagnostics."""
import queue
import threading
import json
import os
from pathlib import Path
import sqlite3
from evidence_common import connection
import subprocess
import shutil
import tempfile
import unittest
from evidence_common import Fixture, clean_env
from install_local import install, merge_hooks
from retention import plan, purge

BINARY=Path(os.environ.get('IRONMEM_TEST_BINARY','target/debug/ironmem.exe' if os.name=='nt' else 'target/debug/ironmem')).resolve()
SOURCE=Path(__file__).resolve().parent.parent


class Tools(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='ironmem-tools-')
        self.root=Path(self.temp.name)
        self.fixture=Fixture(self.root/'fixture',BINARY)
        self.fixture.run('remember','private retention test canary','--project',str(self.fixture.project))
    def tearDown(self): self.temp.cleanup()

    def test_retention_stale_hold_lock_and_purge(self):
        dbpath=self.fixture.database
        first=plan(dbpath)
        with connection(dbpath) as db: db.execute('UPDATE memory_meta SET legal_hold=1')
        with self.assertRaisesRegex(ValueError,'changed'): purge(dbpath,first['plan_sha256'],True,True)
        held=plan(dbpath)
        with self.assertRaisesRegex(ValueError,'legal holds'): purge(dbpath,held['plan_sha256'],True,True)
        with connection(dbpath) as db: db.execute('UPDATE memory_meta SET legal_hold=0')
        current=plan(dbpath)
        with self.assertRaisesRegex(ValueError,'requires'): purge(dbpath,current['plan_sha256'],False,True)
        with connection(dbpath) as writer:
            writer.execute('BEGIN IMMEDIATE')
            with self.assertRaises(sqlite3.OperationalError): purge(dbpath,current['plan_sha256'],True,True)
            writer.rollback()
        report=purge(dbpath,current['plan_sha256'],True,True)
        self.assertEqual(report['remaining_application_tables'],0)
        self.assertEqual(report['compaction'],'complete')
        self.assertNotIn(b'private retention test canary',dbpath.read_bytes())
        self.fixture.run('snapshot','list')
        with connection(dbpath) as db: self.assertEqual(db.execute('SELECT COUNT(*) FROM memories').fetchone()[0],0)

    def test_install_preserves_hooks_idempotent_and_backup(self):
        prefix=self.root/'install'; claude=self.root/'claude'; claude.mkdir()
        unrelated={'hooks':{'Stop':[{'hooks':[{'type':'command','command':'echo keep-me'}]}]},'theme':'dark'}
        (claude/'settings.json').write_text(json.dumps(unrelated))
        first=install(SOURCE,prefix,claude,BINARY)
        self.assertGreater(first['changed_files'],0)
        second=install(SOURCE,prefix,claude,BINARY)
        self.assertEqual(second['changed_files'],0)
        self.assertIsNone(second['backup'])
        if os.name!='nt': self.assertIn('keep-me',(claude/'settings.json').read_text())
        config=prefix/'settings.json'
        config.write_text(self.fixture.config.read_text())
        (prefix/'mcp.json').write_text('{}')
        upgraded=install(SOURCE,prefix,claude,BINARY)
        backup=Path(upgraded['backup'])/'memory.db'
        with connection(backup) as copied:
            self.assertEqual(copied.execute('SELECT COUNT(*) FROM memories').fetchone()[0],1)
            self.assertEqual(copied.execute('PRAGMA integrity_check').fetchone()[0],'ok')
        self.assertEqual(json.loads(config.read_text()),json.loads(self.fixture.config.read_text()))

    @unittest.skipIf(os.name=='nt','Unix streamed installer')
    def test_streamed_installer_bootstrap_and_cleanup(self):
        mockbin=self.root/'mockbin'; mockbin.mkdir()
        source=self.root/'bootstrap-source'; source.mkdir()
        (source/'scripts').mkdir()
        for name in ('install_local.py','evidence_common.py'):
            shutil.copy2(SOURCE/'scripts'/name,source/'scripts'/name)
        marker=self.root/'clone-path'
        git=mockbin/'git'
        # Mock only the remote clone transport; execute the real installer.
        git.write_text('#!/usr/bin/env python3\nimport pathlib,shutil,sys\nassert sys.argv[1:5]==["clone","--depth","1","https://github.com/BMC-INC/Iron-mem.git"]\n'
                       +f'shutil.copytree({str(source)!r},sys.argv[5],dirs_exist_ok=True)\npathlib.Path({str(marker)!r}).write_text(sys.argv[5])\n')
        git.chmod(0o755)
        env=clean_env(); env['PATH']=str(mockbin)+os.pathsep+env['PATH']
        command=['bash','-s','--','--binary',str(BINARY),'--prefix',str(self.root/'streamed'),'--claude-dir',str(self.root/'streamed-claude'),'--no-hooks']
        result=subprocess.run(command,input=(SOURCE/'install.sh').read_text(),env=env,cwd=self.root,capture_output=True,text=True,timeout=60)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertFalse(Path(marker.read_text()).exists())
        self.assertTrue((self.root/'streamed'/'bin'/'ironmem').exists())

    def test_legacy_hook_merge_preserves_mixed_group(self):
        hookdir=Path('/fixture/hooks')
        original={'hooks':{'Stop':[{'type':'command','command':str(hookdir/'stop.sh')},
                                  {'matcher':'test','hooks':[{'type':'command','command':'echo retained'}]}]}}
        merged=merge_hooks(original,hookdir)
        self.assertEqual(len(merged['hooks']['Stop']),2)
        self.assertEqual(merged['hooks']['Stop'][0]['matcher'],'test')
        self.assertEqual(merge_hooks(merged,hookdir),merged)

    def test_newcomer_mcp_save_search_export_restore(self):
        # The same protocol a new assistant uses, with no personal settings.
        messages=[{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'rehearsal','version':'1'}}},
                  {'jsonrpc':'2.0','method':'notifications/initialized'},
                  {'jsonrpc':'2.0','id':2,'method':'tools/list','params':{}},
                  {'jsonrpc':'2.0','id':3,'method':'tools/call','params':{'name':'memory_diagnose','arguments':{'request':{'namespace':'local','project':str(self.fixture.project),'query':'canary'}}}}]
        with tempfile.TemporaryFile(mode='w+') as errors:
            process=subprocess.Popen([str(BINARY),'--config',str(self.fixture.config),'mcp'],
                                     env=clean_env(),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=errors,text=True)
            replies=queue.Queue()
            reader=threading.Thread(target=lambda: [replies.put(line) for line in process.stdout],daemon=True)
            reader.start()
            def send(message):
                process.stdin.write(json.dumps(message)+'\n'); process.stdin.flush()
                if 'id' not in message: return None
                while True:
                    response=json.loads(replies.get(timeout=60))
                    if response.get('id')==message['id']:
                        self.assertIn('result',response)
                        self.assertFalse(response['result'].get('isError',False))
                        return response['result']
            try:
                send(messages[0]); send(messages[1])
                listed=send(messages[2])
                self.assertIn('memory_diagnose',[t['name'] for t in listed['tools']])
                report=send(messages[3])
                self.assertTrue(json.loads(report['content'][0]['text'])['context'] is None)
            finally:
                process.kill(); process.wait(timeout=10); reader.join(timeout=5)
                process.stdin.close(); process.stdout.close()
        diagnosis,_=self.fixture.diagnose('canary')
        self.assertIn('private retention test canary',diagnosis['context'])
        self.fixture.run('snapshot','create','--project',str(self.fixture.project),'--label','onboarding')
        with connection(self.fixture.database) as db:
            snapshot=db.execute("SELECT id FROM brain_snapshots WHERE label='onboarding'").fetchone()[0]
        exported=self.root/'checkpoint.json'
        self.fixture.run('snapshot','export',snapshot,exported)
        other=Fixture(self.root/'restored',BINARY)
        other.run('snapshot','import',exported,'--dry-run')
        other.run('snapshot','import',exported)
        with connection(other.database) as db:
            self.assertEqual(db.execute('SELECT summary FROM memories').fetchone()[0],'private retention test canary')


if __name__=='__main__': unittest.main()
