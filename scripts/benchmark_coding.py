#!/usr/bin/env python3
"""Paired executable coding tasks. Default adapter is a harness check, not AI evidence."""
import argparse
import json
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile
import time
from evidence_common import Fixture, clean_env, connection, digest, provenance, write_report


def invoke(spec, payload, timeout):
    command = spec['command']
    if not isinstance(command, list) or not command or not all(isinstance(x, str) for x in command):
        raise ValueError('adapter command must be a nonempty argv array')
    result = subprocess.run(command, input=json.dumps(payload), capture_output=True, text=True,
                            timeout=timeout, env=clean_env())
    if result.returncode:
        raise RuntimeError(f'adapter failed with exit {result.returncode}: {result.stderr[-1000:]}')
    return json.loads(result.stdout)


def bounded(text, budget):
    return text.encode()[:budget].decode('utf-8', errors='ignore')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/ironmem'))
    parser.add_argument('--tasks', type=Path, default=Path('tests/fixtures/coding/tasks.json'))
    parser.add_argument('--agent', type=Path, help='trusted local JSON adapter descriptor with command and model identity')
    parser.add_argument('--retriever', type=Path, action='append', default=[], help='competitor adapter descriptor: name, version, command')
    parser.add_argument('--budgets', default='2000,4000,8000')
    parser.add_argument('--repeats', type=int, default=1)
    parser.add_argument('--timeout', type=int, default=120)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    budgets = list(map(int, args.budgets.split(',')))
    if not budgets or any(b < 2000 or b > 24000 for b in budgets) or not 1 <= args.repeats <= 100:
        parser.error('budgets must be 2000..24000; repeats 1..100')
    fixture_bytes = args.tasks.read_bytes()
    tasks = json.loads(fixture_bytes)['tasks']
    runner_sha = digest(Path(__file__).read_bytes())
    agent = json.loads(args.agent.read_text()) if args.agent else None
    if agent and (not agent.get('model') or not agent.get('version')):
        parser.error('agent descriptor requires model and version')
    retrievers = [json.loads(p.read_text()) for p in args.retriever]
    names = ['no_memory','full_history','ironmem'] + [r['name'] for r in retrievers]
    if len(set(names)) != len(names): parser.error('duplicate arm names')
    runs = []
    with tempfile.TemporaryDirectory(prefix='ironmem-coding-') as temp:
        pinned_binary = Path(temp)/('ironmem.exe' if sys.platform=='win32' else 'ironmem')
        shutil.copy2(args.binary,pinned_binary)
        args.binary = pinned_binary
        binary_provenance = provenance(pinned_binary)
        # Each task has an isolated namespace/project database, avoiding cross-task leakage.
        for task_index, task in enumerate(tasks):
            task_store = Fixture(Path(temp)/f'task-{task_index}', args.binary)
            for memory in task['memories']:
                task_store.run('remember',memory,'--project',str(task_store.project),'--tags',task['query'])
            # Freeze the synthetic render date; no wall-clock text enters the model prompt.
            with connection(task_store.database) as db:
                db.execute('UPDATE memories SET created_at=?',(1788825600,))
            for budget in budgets:
                for repeat in range(args.repeats):
                    arms = names.copy()
                    random.Random(f'{task["id"]}:{budget}:{repeat}').shuffle(arms)
                    for arm in arms:
                        started = time.perf_counter()
                        record = {'task':task['id'],'category':task['category'],'arm':arm,'budget_bytes':budget,'repeat':repeat,'passed':False}
                        stage='retrieval'
                        try:
                            retrieval_started = time.perf_counter()
                            if arm == 'no_memory': context = ''
                            elif arm == 'full_history': context = bounded('\n'.join(task['memories']),budget)
                            elif arm == 'ironmem':
                                diagnosis, _ = task_store.diagnose(task['query'],budget)
                                context = diagnosis['context']
                                record['memory_ids'] = diagnosis['render']['written_ids']
                            else:
                                spec = next(r for r in retrievers if r['name']==arm)
                                context = invoke(spec,{'protocol':1,'case_id':task['id'],'memories':task['memories'],'query':task['query'],'budget_bytes':budget,'reset':True},args.timeout)['context']
                            if len(context.encode()) > budget: raise ValueError('retriever exceeded context budget')
                            record['retrieval_ms']=(time.perf_counter()-retrieval_started)*1000
                            record['context_bytes']=len(context.encode())
                            record['context_sha256']=digest(context.encode())
                            payload={'protocol':1,'instruction':task['instruction'],'files':task.get('files',{}),
                                     'context':context,'seed':repeat,'model':agent['model'] if agent else 'deterministic-harness-only',
                                     'settings':agent.get('settings',{}) if agent else {}}
                            # Hidden checks and expected solutions are never passed to an external adapter.
                            stage='agent'
                            if agent: output=invoke(agent,payload,args.timeout)
                            else: output={'files':{'solution.py':task['smoke_solution'] if context else task['baseline_solution']}}
                            record['agent_measurement']=output.get('measurement')
                            stage='output_contract'
                            files=output['files']
                            if set(files) != {'solution.py'} or not isinstance(files['solution.py'],str):
                                raise ValueError('adapter must return exactly solution.py')
                            if len(files['solution.py'].encode()) > 100000: raise ValueError('solution too large')
                            record['solution']=files['solution.py']
                            with tempfile.TemporaryDirectory(dir=temp) as work:
                                root=Path(work)
                                for name, content in task.get('files',{}).items():
                                    if Path(name).name != name: raise ValueError('fixture path must be a basename')
                                    (root/name).write_text(content)
                                (root/'solution.py').write_text(files['solution.py'])
                                (root/'check.py').write_text(task['tests'])
                                stage='executable_check'
                                result=subprocess.run([sys.executable,'-B','check.py'],cwd=root,env=clean_env(),capture_output=True,text=True,timeout=args.timeout)
                                record['passed']=result.returncode==0
                                record['failure']=None if record['passed'] else result.stderr[-3000:]
                                record['solution_sha256']=digest(files['solution.py'].encode())
                        except (Exception,) as error:
                            record['failure']=str(error)
                            record['passed']=None
                            record['error_stage']=stage
                        record['wall_ms']=(time.perf_counter()-started)*1000
                        runs.append(record)
                        print(f"{task['id']} {arm}: {record['passed']}",file=sys.stderr,flush=True)
    summary={arm:{'passed':sum(r['passed'] is True for r in runs if r['arm']==arm),'total':sum(r['arm']==arm for r in runs),'scored':sum(r['arm']==arm and r['passed'] is not None for r in runs),'errors':sum(r['arm']==arm and r['passed'] is None for r in runs)} for arm in names}
    paired={}
    if agent:
        for arm in names:
            if arm=='no_memory': continue
            differences=[]
            for task in tasks:
                pairs=[]
                for budget in budgets:
                    for repeat in range(args.repeats):
                        subset=[r for r in runs if r['task']==task['id'] and r['budget_bytes']==budget and r['repeat']==repeat]
                        base=next(r for r in subset if r['arm']=='no_memory')
                        candidate=next(r for r in subset if r['arm']==arm)
                        if base['passed'] is not None and candidate['passed'] is not None:
                            pairs.append(int(candidate['passed'])-int(base['passed']))
                if pairs: differences.append(sum(pairs)/len(pairs))
            if differences:
                rng=random.Random(20260907)
                bootstrap=sorted(sum(rng.choice(differences) for _ in differences)/len(differences) for _ in range(2000))
                paired[arm]={'mean_task_pass_difference':sum(differences)/len(differences),
                             'task_cluster_bootstrap_95_percent':[bootstrap[50],bootstrap[1949]],'tasks':len(differences),
                             'note':'Exploratory interval; four tasks provide very weak population evidence.'}
    write_report(args.out,{'schema':1,'measurement':'external_agent_coding_tasks' if agent else 'deterministic_harness_validation',
                          'competitive_superiority_established':False,'fixture_clock_utc':'2026-09-08T00:00:00Z','agent':agent,'retrievers':retrievers,
                          'fixture_sha256':digest(fixture_bytes),'runner_sha256':runner_sha,
                          'provenance':binary_provenance,'summary':summary,'runs':runs,'paired_vs_no_memory':paired,
                          'limitations':['Four small synthetic tasks are not a representative coding benchmark.',
                                        'Harness adapter uses fixture solutions; its scores are not model accuracy.',
                                        'Context budget excludes the identical task prompt and starter files.',
                                        'No hidden tests or answer keys are supplied to external adapters.',
                                        'Wall time includes process startup; avoided work is a reuse check, not human time saved.']})
    # Model task failures are measured outcomes. Harness infrastructure failures are fatal.
    if not agent and any(not r['passed'] for r in runs if r['arm'] in ('ironmem','full_history')):
        raise SystemExit('harness validation failed; see report')


if __name__=='__main__': main()
