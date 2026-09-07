#!/usr/bin/env python3
"""Plot raw density reports with matplotlib; never invent absent accuracy values."""
import argparse
import json
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--storage', nargs='+', type=Path, required=True)
parser.add_argument('--frontier', type=Path)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
reports = [json.loads(path.read_text()) for path in args.storage]
fig, ax = plt.subplots(figsize=(8, 5), layout='constrained')
for path, report in zip(args.storage, reports):
    x = report['load_p95_us']/1000
    y = report['unique_payload_bytes']/1024**2
    ax.scatter([x], [y])
    ax.annotate(path.parent.name, (x, y), xytext=(5, 5), textcoords='offset points')
ax.set(xlabel='Warm-inclusive p95 full-load wall time (ms)', ylabel='Unique compressed payload + manifests (MiB)',
       title='Storage / latency tradeoff — deterministic synthetic corpus')
ax.grid(alpha=.25)
fig.savefig(args.out/'storage-latency.svg')
plt.close(fig)
fig, ax = plt.subplots(figsize=(8, 5), layout='constrained')
for path, report in zip(args.storage, reports):
    rows = report['measurements']
    ax.plot([r['budget_bytes']/1000 for r in rows], [r['total_exposure_bytes']/1000 for r in rows], marker='o', label=path.parent.name)
ax.set(xlabel='Injection budget (decimal KB)', ylabel='Injection + retrieval + expansion payload (decimal KB)', title='Total exposure includes the fixed expansion probe')
ax.legend(); ax.grid(alpha=.25)
fig.savefig(args.out/'total-exposure.svg')
plt.close(fig)
if args.frontier:
    frontier = json.loads(args.frontier.read_text())
    fig, ax = plt.subplots(figsize=(8, 5), layout='constrained')
    scored = [r for r in frontier['budgets'] if r['accuracy'] is not None]
    if scored:
        ax.plot([r['budget_bytes']/1000 for r in scored], [r['accuracy'] for r in scored], marker='o')
        ax.set_ylim(0, 1)
    else:
        ax.text(.5, .5, 'Unscored: no accuracy measurements', ha='center', va='center', transform=ax.transAxes)
    ax.set(xlabel='Injection budget (decimal KB)', ylabel='Accuracy', title=frontier['suite'])
    fig.savefig(args.out/'accuracy-context.svg')
