#!/usr/bin/env python3
"""Actions matrix planning and a host-separated artifact index."""
import argparse
import json
import os
from pathlib import Path

import report
import verdicts
import highlights


def matrix(suite):
    if suite=='compare': return {'include':[{'shard':'memory','backends':'memory'},{'shard':'redis','backends':'redis'}]}
    if suite=='smoke': return {'include':[{'shard':'smoke','backends':'memory redis'}]}
    if suite=='defect': return {'include':[{'shard':'defect','backends':'memory'}]}
    raise ValueError('Unknown benchmark suite')


def aggregate(root, suite):
    """One short overview. Detailed round tables stay in shard artifacts."""
    shards=[]
    sections=[]
    for item in matrix(suite)['include']:
        shard=item['shard']
        folder=root/('benchmark-diagnostics-'+shard)
        status=json.loads((folder/'status.json').read_text()) if (folder/'status.json').exists() else {}
        env=json.loads((folder/'environment.json').read_text()) if (folder/'environment.json').exists() else {}
        raw=json.loads((folder/'raw.json').read_text()) if (folder/'raw.json').exists() else {'rounds':[]}
        platforms=sorted({r['platform_id'] for r in raw['rounds'] if r.get('platform_id')})
        classification_path=folder/'result-classification.json'
        classification=json.loads(classification_path.read_text()) if classification_path.exists() else {}
        semantic=classification.get('status')
        environmental=semantic=='inconclusive_environment'
        complete=(status.get('outcome')=='success' and (bool(raw['rounds']) or environmental)
                  and bool(env) and (folder/'assessment.md').exists())
        if suite=='compare':
            complete=(complete and (folder/'verdicts.json').exists()
                      and (len(platforms)==1 or (environmental and not platforms)))
        if semantic in ('measurement_failure','failure'):
            complete=False
        entry=dict(shard=shard,outcome=status.get('outcome','missing'),complete=complete,
                   verification=semantic or 'unknown',
                   platforms=platforms,git_sha=env.get('git_sha'),
                   rounds=len(raw['rounds']),artifact=folder.name)
        shards.append(entry)
        rows=[{**r,'invalid_reasons':report.validate(r),
               'load_failure_reasons':report.load_failures(r)} for r in raw['rounds']]
        plan_path=folder/'campaign-plan.json'
        plan=json.loads(plan_path.read_text()) if plan_path.exists() else {}
        verdict_path=folder/'verdicts.json'
        verdict=json.loads(verdict_path.read_text()) if verdict_path.exists() else verdicts.analyze(rows)
        outcome=semantic or ('completed' if complete else 'measurement_failure')
        if not semantic and complete and any(r['invalid_reasons'] and r.get('variant') not in highlights.CORE for r in rows):
            outcome='completed_with_peer_exclusions'
        sections += ['## '+shard.capitalize(), '',
                     highlights.concise(rows,verdict,outcome,plan.get('issues',[]),
                                        recoveries=plan.get('environment_recoveries',[]),
                                        unresolved=plan.get('environment_unresolved',[])),
                     'Artifact: `'+folder.name+'`.']
    shas={s['git_sha'] for s in shards if s['git_sha']}
    success=all(s['complete'] for s in shards) and len(shas)==1
    incomplete_evidence=any(x['verification']=='inconclusive_environment' for x in shards)
    status=('Incomplete — inspect failing shard' if not success else
            'Inconclusive — one or more runner-limited shards' if incomplete_evidence else
            'Complete with warnings' if any(x['verification'] in ('completed_with_environment_limits','completed_with_peer_exclusions') for x in shards)
            else 'Complete')
    lines=['# Benchmark results','',f'**Run status:** {status}.',
           'Measurements from different hosted runners are **not combined or ranked**.',
           'Successful execution does not imply a performance winner or maximum throughput.',
           '']+sections
    return dict(suite=suite,complete=success,shards=shards), '\n'.join(lines)+'\n'

def main():
    p=argparse.ArgumentParser(description=__doc__)
    sub=p.add_subparsers(dest='command',required=True)
    plan=sub.add_parser('plan');plan.add_argument('--suite',choices=['compare','smoke','defect'],required=True)
    plan.add_argument('--github-output',action='store_true')
    status=sub.add_parser('status');status.add_argument('folder',type=Path);status.add_argument('--outcome',required=True)
    merge=sub.add_parser('aggregate');merge.add_argument('root',type=Path);merge.add_argument('--suite',choices=['compare','smoke','defect'],required=True)
    merge.add_argument('--output',type=Path,required=True);merge.add_argument('--github-summary',action='store_true')
    a=p.parse_args()
    if a.command=='plan':
        data=json.dumps(matrix(a.suite))
        print(data)
        if a.github_output:
            with open(os.environ['GITHUB_OUTPUT'],'a') as f: f.write('matrix='+data+'\n')
    elif a.command=='status':
        a.folder.mkdir(parents=True,exist_ok=True)
        (a.folder/'status.json').write_text(json.dumps(dict(outcome=a.outcome),indent=2))
    else:
        data,content=aggregate(a.root,a.suite)
        a.output.mkdir(parents=True,exist_ok=True)
        (a.output/'index.json').write_text(json.dumps(data,indent=2))
        (a.output/'overview.md').write_text(content)
        if a.github_summary:
            with open(os.environ['GITHUB_STEP_SUMMARY'],'a') as f: f.write(content[:200000])
        if not data['complete']: raise SystemExit('One or more benchmark shards are incomplete or failed')


if __name__=='__main__': main()
