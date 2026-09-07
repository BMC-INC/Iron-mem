#!/usr/bin/env python3
"""Run paired budget experiments. No answer/judge calls unless --scored is explicit."""
import argparse
import json
import math
from pathlib import Path
import subprocess

BUDGETS = [2000, 4000, 8000, 16000, 24000, 48000, 96000]


def paired_interval(differences):
    """Distribution-free 95% Hoeffding bound for paired outcomes in [-1, 1]."""
    mean = sum(differences) / len(differences)
    radius = math.sqrt(2 * math.log(40) / len(differences))
    return mean, max(-1, mean-radius), min(1, mean+radius)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/ironmem'))
    parser.add_argument('--suite', choices=['longmemeval', 'locomo'], required=True)
    parser.add_argument('--data', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--scored', action='store_true')
    parser.add_argument('--answer-model')
    parser.add_argument('--judge-model')
    parser.add_argument('--per-category', type=int, default=1)
    args = parser.parse_args()
    if args.scored and not (args.answer_model and args.judge_model):
        parser.error('--scored requires explicit --answer-model and --judge-model')
    if args.per_category < 1:
        parser.error('--per-category must be positive')
    args.out.mkdir(parents=True, exist_ok=True)
    common = [str(args.binary.resolve()), 'bench', args.suite, '--data', str(args.data.resolve()),
              '--stratified-per-ability', str(args.per_category)]
    if not args.scored:
        common += ['--dry-run']
    else:
        common += ['--answer-model', args.answer_model, '--judge-model', args.judge_model]
    runs = {}
    for label in ['full-context'] + BUDGETS:
        out = args.out / str(label)
        extra = ['--full-context'] if label == 'full-context' else ['--injection-budget-bytes', str(label)]
        subprocess.run(common + ['--out', str(out)] + extra, check=True)
        checkpoint = out / 'longmemeval-checkpoint'
        rows = [json.loads(p.read_text()) for p in sorted(checkpoint.glob('*.json')) if p.name != 'manifest.json']
        if not rows:
            raise RuntimeError(f'No questions in {out}')
        if any(r['hypothesis'].startswith('[harness error:') for r in rows):
            raise RuntimeError(f'Harness failures in {out}; refusing a passing frontier')
        runs[str(label)] = {r['question_id']: r for r in rows}
    baseline = runs['full-context']
    result = {'schema': 1, 'suite': args.suite, 'scored': args.scored,
              'criterion': 'paired overall accuracy delta lower 95% bound >= -0.01; category regressions require review',
              'interval': 'distribution-free Hoeffding paired bound; conservative and often inconclusive',
              'quality_scope': 'renderer context only; fixed expansion probe reported by ironmem density separately',
              'budgets': []}
    for budget in BUDGETS:
        rows = runs[str(budget)]
        if rows.keys() != baseline.keys():
            raise RuntimeError('Paired question IDs differ')
        entry = {'budget_bytes': budget, 'n': len(rows),
                 'mean_context_bytes': sum(r['context_bytes'] for r in rows.values())/len(rows),
                 'accuracy': None, 'accuracy_delta': None, 'confidence_interval': None, 'gate': 'unscored', 'categories': {}}
        if args.scored:
            differences = [int(r['correct'])-int(baseline[q]['correct']) for q, r in rows.items()]
            delta, low, high = paired_interval(differences)
            entry.update(accuracy=sum(r['correct'] for r in rows.values())/len(rows), accuracy_delta=delta,
                         confidence_interval=[low, high], gate='noninferior' if low >= -0.01 else 'regressed' if high < -0.01 else 'inconclusive')
            for ability in sorted({r['ability'] for r in rows.values()}):
                group = [r for r in rows.values() if r['ability'] == ability]
                entry['categories'][ability] = {'n': len(group), 'accuracy': sum(r['correct'] for r in group)/len(group)}
        result['budgets'].append(entry)
    (args.out/'frontier.json').write_text(json.dumps(result, indent=2)+'\n')
    with (args.out/'frontier.csv').open('w') as handle:
        handle.write('budget_bytes,n,mean_context_bytes,accuracy,gate\n')
        for row in result['budgets']:
            score='' if row['accuracy'] is None else row['accuracy']
            handle.write(f"{row['budget_bytes']},{row['n']},{row['mean_context_bytes']},{score},{row['gate']}\n")


if __name__ == '__main__':
    main()
