#!/usr/bin/env python3
"""Probe and confirm a backend on one host; never carry a boundary to another runner."""
import argparse
import json
import math
from pathlib import Path
import signal
import subprocess
import sys

import report
import summary
import variants
import verdicts
import capacity
import workloads

HERE=Path(__file__).resolve().parent
PEERS=['baseline','head','governor','trypema','extensible']
PROBES=capacity.RANGES


def confirmation_plan(rows, backend, peers):
    """Confirm at most HEAD's adjacent pass/fail endpoints, with every peer present."""
    valid_baseline={r['target_rps'] for r in rows if r['variant']=='baseline' and verdicts.passed(r)}
    best={}
    for peer in peers:
        if peer=='baseline': continue
        rates=[r['target_rps'] for r in rows if r['variant']==peer and r['target_rps'] in valid_baseline and verdicts.passed(r)]
        if rates: best[peer]=max(rates)
    if 'head' not in best:
        return [],dict(unresolved=sorted(set(peers)-{'baseline'}),reason='HEAD has no clean probe level within the calibrated baseline range')
    bounds=capacity.probe_bounds(rows,peers)
    if bounds['head']['non_monotonic']:
        return [],dict(reason='Non-monotonic HEAD probe outcomes; no stable bracket can be claimed',probe_bounds=bounds)
    levels=[best['head']]
    if bounds['head']['upper'] is not None: levels.append(bounds['head']['upper'])
    # A jointly sustainable probe point provides latency and CPU comparisons.
    common=set(valid_baseline)
    for peer in peers:
        if peer!='baseline': common &= {r['target_rps'] for r in rows if r['variant']==peer and verdicts.passed(r)}
    return [dict(rate=rate,variants=peers) for rate in sorted(levels)],dict(
        backend=backend,probe_candidates=best,common_probe_rate=max(common,default=None),
        probe_bounds=bounds,
        unresolved=sorted(v for v,b in bounds.items() if v!='baseline' and (b['lower'] is None or b['upper'] is None or b['non_monotonic'])),
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
        if (output/'campaign-plan.json').exists():
            env['campaign']['plan']=json.loads((output/'campaign-plan.json').read_text())
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
    p.add_argument('--full-matrix',action='store_true',help='Use all nine client/mode combinations instead of five representative cases')
    p.add_argument('--resolution',type=int,help='Stop refining once pass/fail gap is within this many RPS')
    p.add_argument('--refine-budget',type=int,default=3,help='Maximum additional short rate probes (0..6)')
    p.add_argument('--plan',action='store_true')
    a=p.parse_args()
    rates=sorted(set(a.probe_rates or ([15000] if a.check else PROBES[a.backend])))
    if any(not 1000<=rate<=100000 for rate in rates) or len(rates)>6:
        p.error('Use at most six probe rates in 1000..100000')
    resolution=capacity.RESOLUTION[a.backend] if a.resolution is None else a.resolution
    if not 500<=resolution<=20000 or not 0<=a.refine_budget<=6:
        p.error('Resolution must be 500..20000 RPS and refinement budget 0..6')
    peers=[v for v in PEERS if variants.supports(v,a.backend,'allow')]
    if a.check: peers=['baseline','head'];rates=rates[:1]
    probe_seconds,warmup,seconds,repeats=(2,1,2,1) if a.check else (5,5,30,5)
    cases=workloads.cases(a.full_matrix)
    if a.plan:
        calibration_rounds=0 if a.check else capacity.LOADGEN_STEPS
        short_rounds=(len(rates)+(0 if a.check else a.refine_budget)+2*(len(cases)-1))*len(peers)+calibration_rounds
        print(json.dumps(dict(backend=a.backend,variants=peers,probe_rates=rates,
                             operational_check=a.check,probe_seconds=probe_seconds,probe_warmup=1 if a.check else 2,confirmation_seconds=seconds,
                             confirmation_warmup=warmup,confirmation_repeats=repeats,
                             cases=cases,resolution_rps=resolution,refinement_budget=0 if a.check else a.refine_budget,
                             calibration_probes=calibration_rounds,loadgen_headroom=capacity.LOADGEN_HEADROOM,
                             max_confirmation_rounds=(len(cases)+1)*len(peers)*repeats,
                             max_confirmation_load_seconds=(len(cases)+1)*len(peers)*repeats*(seconds+warmup),
                             max_short_rounds=short_rounds,
                             note='Upper bounds, not scheduled counts. Builds/startup/diagnostics/short probes are additional; extra reject sampling may be longer. One cold build; later phases verify provenance.'),indent=2));return
    if a.output.exists() and any(child.name!='run.log' for child in a.output.iterdir()):
        p.error('Use a fresh campaign output directory; old results must not be reused')
    a.output.mkdir(parents=True,exist_ok=True)
    children=[];status='failure';trace=[];issues=[];coverage=[]
    manifest=None
    def interrupted(*_): raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM,interrupted)
    def execute(name,stage,selected,levels,seconds,warmup,repeats,clients=1,mode='allow',record=True):
        nonlocal manifest
        child=a.output/name
        if record: children.append((child,stage))
        cmd=[sys.executable,str(HERE/'bench.py'),'compare','--backends',a.backend,
             '--variants',*selected,'--rates',*map(str,levels),'--duration',str(seconds),
             '--warmup',str(warmup),'--repeats',str(repeats),'--output',str(child)]
        cmd += ['--clients',str(clients),'--modes',mode]
        if manifest: cmd += ['--reuse-build-manifest',str(manifest)]
        if a.allow_dirty: cmd.append('--allow-dirty')
        # The first phase builds; later phases verify exact source and image provenance.
        process=subprocess.Popen(cmd)
        try:
            if process.wait(): raise RuntimeError('Benchmark phase failed: '+name)
        finally:
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: process.kill();process.wait()
        if manifest is None: manifest=child/'environment.json'
        return json.loads((child/'raw.json').read_text())['rounds']
    def probe_quietly(name,levels):
        """Short baseline probe that tolerates the invalid-baseline abort."""
        nonlocal manifest
        try: return execute(name,'probe',['baseline'],levels,probe_seconds,2,1,record=False)
        except RuntimeError:
            raw=a.output/name/'raw.json';env=a.output/name/'environment.json'
            # Reuse the build only when images were actually recorded; otherwise rebuild.
            manifest=env if env.exists() and json.loads(env.read_text()).get('images') else None
            return json.loads(raw.read_text())['rounds'] if raw.exists() else []
    try:
        calibration=dict(initial=list(rates),steps=[],adjusted=False)
        # Explicit --probe-rates are honoured; only the default ladder self-calibrates.
        if not a.check and a.probe_rates is None:
            for step in range(capacity.LOADGEN_STEPS):
                observed=probe_quietly('calibrate-'+str(step),[rates[-1]])
                decision=capacity.ladder_step(rates,observed,resolution)
                calibration['steps'].append(decision or dict(rate=rates[-1],pressure=False))
                if decision is None: break
                rates=decision['rates']
        calibration['final']=list(rates)
        calibration['adjusted']=rates!=calibration['initial']
        anchor=execute('probe','probe',peers,rates,probe_seconds,1 if a.check else 2,1)
        for i in range(0 if a.check else a.refine_budget):
            decision=capacity.next_probe(anchor,peers,resolution)
            if decision is None: break
            trace.append(decision)
            anchor += execute('refine-'+str(i),'probe',peers,[decision['rate']],probe_seconds,2,1)
        collect(a.output,children,a.check)
        plan,details=confirmation_plan(anchor,a.backend,peers)
        details['calibration']=calibration
        details['search_status']=capacity.search_status(anchor,peers,resolution,len(trace),0 if a.check else a.refine_budget,a.check)
        def save_plan():
            (a.output/'campaign-plan.json').write_text(json.dumps(dict(confirmations=plan,refinement_trace=trace,
                resolution_rps=resolution,coverage_cases=coverage,issues=issues,**details),indent=2))
        save_plan()
        if not plan: raise RuntimeError('No confirmation plan: inspect probe diagnostics')
        for cell in plan:
            execute('confirm-'+str(cell['rate']),'confirm',cell['variants'],[cell['rate']],seconds,warmup,repeats)
            collect(a.output,children,a.check)
        # Compare cardinality and response paths at one conservative shared offered level.
        reference=details['common_probe_rate'] or min(details['probe_candidates'].values())
        rate=max(5000,round(reference*.8/500)*500) if not a.check else rates[0]
        for case in cases:
            clients,mode=case['clients'],case['mode']
            if clients==1 and mode=='allow': continue
            offered=rate;name=f'{clients}-{mode}'
            for attempt in range(2):
                duration=min(60,max(probe_seconds,math.ceil(clients*10/offered))) if mode=='reject' else probe_seconds
                observed=execute('survey-'+name+'-'+str(attempt),'probe',peers,[offered],duration,1 if a.check else 2,1,clients,mode)
                if all(verdicts.passed(r) for r in observed) or attempt==1: break
                fallback=max(5000,round(offered/1000)*500)
                if fallback==offered: break
                offered=fallback
            eligible=[r['variant'] for r in observed if not report.validate(r) and not workloads.coverage_failures(r)]
            skipped=sorted(set(peers)-set(eligible))
            entry=dict(clients=clients,mode=mode,rate=offered,variants=eligible,skipped=skipped)
            coverage.append(entry);save_plan()
            if skipped: issues.append(dict(case=name,reason='Invalid measurement or required response/cardinality coverage missing',variants=skipped))
            save_plan()
            if not {'baseline','head'}<=set(eligible):
                issues.append(dict(case=name,reason='No usable baseline/HEAD pair; long repeats skipped'));save_plan();continue
            if not any(r['variant']!='baseline' and verdicts.passed(r) for r in observed):
                issues.append(dict(case=name,reason='No passing implementation after fallback; long repeats skipped'));save_plan();continue
            duration=min(60,max(seconds,math.ceil(clients*10/offered))) if mode=='reject' else seconds
            execute('coverage-'+name,'confirm',eligible,[offered],duration,warmup,repeats,clients,mode)
            collect(a.output,children,a.check)
        save_plan()
        rows=collect(a.output,children,a.check)
        status='completed'
        if issues or any(report.validate(r) for r in rows):
            status='measurement_failure'
    finally:
        collect(a.output,children,a.check)
        (a.output/'assessment.md').write_text(summary.render(a.output,status))
    if status!='completed': raise SystemExit('Campaign incomplete or invalid; inspect workload coverage, bounds and diagnostics')


if __name__=='__main__': main()
