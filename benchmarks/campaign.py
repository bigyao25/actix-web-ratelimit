#!/usr/bin/env python3
"""Probe and confirm a backend on one host; never carry a boundary to another runner."""
import argparse
import json
from pathlib import Path
import signal
import subprocess
import sys

import report
import summary
import variants
import verdicts

HERE=Path(__file__).resolve().parent
PEERS=['baseline','head','governor','trypema','extensible']
PROBES={'memory':[15000,30000,45000,60000],'redis':[10000,15000,20000,25000,30000]}


def confirmation_plan(rows, backend, peers):
    """Select common cost and per-peer boundary candidates, preserving matched controls."""
    valid_baseline={r['target_rps'] for r in rows if r['variant']=='baseline' and verdicts.passed(r)}
    best={}
    for peer in peers:
        if peer=='baseline': continue
        rates=[r['target_rps'] for r in rows if r['variant']==peer and r['target_rps'] in valid_baseline and verdicts.passed(r)]
        if rates: best[peer]=max(rates)
    if 'head' not in best:
        return [],dict(unresolved=sorted(set(peers)-{'baseline'}),reason='HEAD has no clean probe level within the calibrated baseline range')
    selected={}
    # HEAD's candidate is tested by every peer, permitting confirmed pass/fail contrasts.
    selected[best['head']]=set(peers)
    for peer,rate in best.items(): selected.setdefault(rate,set()).update(('baseline','head',peer))
    # A jointly sustainable probe point provides latency and CPU comparisons.
    common=set(valid_baseline)
    for peer in peers:
        if peer!='baseline': common &= {r['target_rps'] for r in rows if r['variant']==peer and verdicts.passed(r)}
    if common: selected.setdefault(max(common),set()).update(peers)
    return [dict(rate=rate,variants=[v for v in peers if v in names]) for rate,names in sorted(selected.items())],dict(
        backend=backend,probe_candidates=best,common_probe_rate=max(common,default=None),
        unresolved=sorted(set(peers)-{'baseline'}-best.keys()),
        note='Probe candidates are not confirmed capacities. No tested pass or no failing upper point leaves the boundary unresolved.')


def collect(output, children, check=False):
    rows=[];envs=[]
    for child,stage in children:
        raw=child/'raw.json';metadata=child/'environment.json'
        if raw.exists(): rows += [{**r,'campaign_stage':stage,'source_run':child.name} for r in json.loads(raw.read_text())['rounds']]
        if metadata.exists(): envs.append((child.name,json.loads(metadata.read_text())))
    if len({r.get('platform_id') for r in rows if r.get('platform_id')})>1:
        raise RuntimeError('Runner identity changed between campaign phases')
    if len({e['git_sha'] for _,e in envs})>1 or len({json.dumps(e['source_hashes'],sort_keys=True) for _,e in envs})>1:
        raise RuntimeError('Source changed between campaign phases')
    (output/'raw.json').write_text(json.dumps(dict(rounds=rows),indent=2))
    if envs:
        env=envs[0][1]
        env['campaign']=dict(phases=[name for name,_ in envs],same_host=True,operational_check=check)
        (output/'environment.json').write_text(json.dumps(env,indent=2))
    result=report.summarize(dict(rounds=rows))
    (output/'validated.json').write_text(json.dumps(result,indent=2))
    conclusions=verdicts.analyze(rows)
    (output/'verdicts.json').write_text(json.dumps(conclusions,indent=2))
    (output/'report.md').write_text(report.markdown(result)+'\n'+verdicts.markdown(conclusions))
    return rows


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--backend',choices=PROBES,required=True)
    p.add_argument('--probe-rates',nargs='+',type=int)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--allow-dirty',action='store_true')
    p.add_argument('--check',action='store_true',help='Short baseline/HEAD orchestration check; never qualifies a performance level')
    p.add_argument('--plan',action='store_true')
    a=p.parse_args()
    rates=sorted(set(a.probe_rates or ([15000] if a.check else PROBES[a.backend])))
    if any(not 1000<=rate<=100000 for rate in rates) or len(rates)>6:
        p.error('Use at most six probe rates in 1000..100000')
    peers=[v for v in PEERS if variants.supports(v,a.backend,'allow')]
    if a.check: peers=['baseline','head'];rates=rates[:1]
    probe_seconds,warmup,seconds,repeats=(2,1,2,1) if a.check else (5,5,30,5)
    if a.plan:
        print(json.dumps(dict(backend=a.backend,variants=peers,probe_rates=rates,
                             operational_check=a.check,probe_seconds=probe_seconds,probe_warmup=1 if a.check else 2,confirmation_seconds=seconds,
                             confirmation_warmup=warmup,confirmation_repeats=repeats,
                             note='Confirmation cells depend on same-runner probes; no cross-runner capacity transfer'),indent=2));return
    if a.output.exists() and any(child.name!='run.log' for child in a.output.iterdir()):
        p.error('Use a fresh campaign output directory; old results must not be reused')
    a.output.mkdir(parents=True,exist_ok=True)
    children=[];status='failure'
    def interrupted(*_): raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM,interrupted)
    def execute(name,stage,selected,levels,seconds,warmup,repeats):
        child=a.output/name;children.append((child,stage))
        cmd=[sys.executable,str(HERE/'bench.py'),'compare','--backends',a.backend,
             '--variants',*selected,'--rates',*map(str,levels),'--duration',str(seconds),
             '--warmup',str(warmup),'--repeats',str(repeats),'--output',str(child)]
        if a.allow_dirty: cmd.append('--allow-dirty')
        # Every phase rebuilds from the recorded source; local Docker caches avoid recompilation.
        process=subprocess.Popen(cmd)
        try:
            if process.wait(): raise RuntimeError('Benchmark phase failed: '+name)
        finally:
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: process.kill();process.wait()
    try:
        execute('probe','probe',peers,rates,probe_seconds,1 if a.check else 2,1)
        rows=collect(a.output,children,a.check)
        plan,details=confirmation_plan(rows,a.backend,peers)
        (a.output/'campaign-plan.json').write_text(json.dumps(dict(confirmations=plan,**details),indent=2))
        if not plan: raise RuntimeError('No confirmation plan: inspect probe diagnostics')
        for cell in plan:
            execute('confirm-'+str(cell['rate']),'confirm',cell['variants'],[cell['rate']],seconds,warmup,repeats)
            collect(a.output,children,a.check)
        rows=collect(a.output,children,a.check)
        status='completed'
        if any(report.validate(r) for r in rows if r.get('campaign_stage')=='confirm'):
            status='measurement_failure'
    finally:
        collect(a.output,children,a.check)
        (a.output/'assessment.md').write_text(summary.render(a.output,status))
    if status!='completed': raise SystemExit('Confirmation has invalid measurements; inspect diagnostics')


if __name__=='__main__': main()
