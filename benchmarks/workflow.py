#!/usr/bin/env python3
"""Actions matrix planning and a host-separated artifact index."""
import argparse
import json
import os
from pathlib import Path


def matrix(suite):
    if suite=='compare': return {'include':[{'shard':'memory','backends':'memory'},{'shard':'redis','backends':'redis'}]}
    if suite=='smoke': return {'include':[{'shard':'smoke','backends':'memory redis'}]}
    if suite=='defect': return {'include':[{'shard':'defect','backends':'memory'}]}
    raise ValueError('Unknown benchmark suite')


def aggregate(root, suite):
    shards=[]
    lines=['# Benchmark workflow overview','',
           'Backends run on independent hosted runners. Each shard compares every supported product and its HTTP baseline sequentially on its own host. Absolute RPS and sample distributions are never pooled across shards.','']
    for item in matrix(suite)['include']:
        shard=item['shard'];folder=root/('benchmark-diagnostics-'+shard)
        status=json.loads((folder/'status.json').read_text()) if (folder/'status.json').exists() else {}
        env=json.loads((folder/'environment.json').read_text()) if (folder/'environment.json').exists() else {}
        raw=json.loads((folder/'raw.json').read_text()) if (folder/'raw.json').exists() else {'rounds':[]}
        platforms=sorted({r['platform_id'] for r in raw['rounds'] if r.get('platform_id')})
        complete=status.get('outcome')=='success' and bool(raw['rounds']) and bool(env) and (folder/'assessment.md').exists()
        if suite=='compare': complete=complete and (folder/'verdicts.json').exists() and len(platforms)==1
        entry=dict(shard=shard,outcome=status.get('outcome','missing'),complete=complete,
                   platforms=platforms,git_sha=env.get('git_sha'),rounds=len(raw['rounds']),
                   artifact=folder.name)
        shards.append(entry)
        lines += ['## '+shard,'',f"Artifact: `{folder.name}`. Benchmark step outcome: **{entry['outcome']}**. Complete diagnostics: **{complete}**. Platforms: `{', '.join(platforms) or 'unknown'}`.",'']
        if (folder/'assessment.md').exists():
            # Embed independently computed results; do not concatenate raw rounds and re-rank.
            content=(folder/'assessment.md').read_text()
            lines += [content.replace('# Benchmark assessment','### Shard assessment',1)]
        else: lines += ['No assessment artifact received; inspect the shard job log.','']
    shas={s['git_sha'] for s in shards if s['git_sha']}
    success=all(s['complete'] for s in shards) and len(shas)==1
    lines.insert(2,'Artifact completeness: **'+('complete' if success else 'incomplete / failed')+'**. This status does not imply a performance winner.')
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
