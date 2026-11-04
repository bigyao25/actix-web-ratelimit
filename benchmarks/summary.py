#!/usr/bin/env python3
"""Evidence-based local/Actions summary. Never infer capacity from paced RPS."""
import argparse
import collections
import json
import os
from pathlib import Path
import report


def expected_historical(round_):
    reasons=report.validate(round_)
    return round_.get('variant')=='v0.1.2' and bool(reasons) and set(reasons)<={'correctness_mismatch','quota_exceeded'}


def assess(round_):
    reasons=report.validate(round_)
    if expected_historical(round_):
        return 'historical correctness defect'
    if round_.get('failure') or 'correctness_mismatch' in reasons or 'quota_exceeded' in reasons:
        return 'failed / correctness error'
    # CPU is a pressure signal, not proof of a causal bottleneck. Server CPU spans warmup too.
    generator_pressure=round_.get('loadgen_cpu_pct',0)>=85*round_.get('loadgen_cpu_limit',1)
    if generator_pressure and (round_.get('loadgen_saturated') or round_.get('dispatch_lag',{}).get('p99_ms',0)>10):
        return 'loadgen pressure; server capacity unresolved'
    if reasons:
        return 'invalid; capacity unresolved'
    if round_.get('scheduled_latency',{}).get('p99_ms',0)>10:
        return 'scheduled p99 exceeds diagnostic 10 ms budget'
    if round_.get('server_cpu_pct',0)>=80*round_.get('server_cpu_limit',2):
        return 'server CPU pressure candidate; needs confirmation'
    return 'no bottleneck demonstrated'


def render(folder, status='unknown', artifact_url=''):
    raw_path=folder/'raw.json'
    env_path=folder/'environment.json'
    env=json.loads(env_path.read_text()) if env_path.exists() else {}
    rows=json.loads(raw_path.read_text()).get('rounds',[]) if raw_path.exists() else []
    rows=[{**r,'invalid_reasons':report.validate(r)} for r in rows]
    valid=[r for r in rows if not r['invalid_reasons']]
    expected=[r for r in rows if expected_historical(r)]
    unexpected=[r for r in rows if r['invalid_reasons'] and not expected_historical(r)]
    esc=lambda v: str(v).replace('|','\\|').replace('\n',' ').replace('`','').replace('<','&lt;')
    fmt=lambda v: 'N/A' if v is None else f'{v:.2f}'
    lines=['# Benchmark assessment','',f'Execution status: **{esc(status)}**.',
           f'Completed rounds: **{len(rows)}**; valid: **{len(valid)}**; expected historical defects: **{len(expected)}**; other invalid: **{len(unexpected)}**.','']
    if not rows:
        lines += ['**No benchmark data collected.** Check build/preflight logs. No performance conclusion is available.','']
    else:
        lines += ['This run checks correctness and cost at offered loads. **No maximum-throughput ranking or cross-device performance claim is made.**','']
        if unexpected:
            lines += ['Unexpected invalid measurements exist; inspect their diagnostics before interpreting results.','']
        if expected:
            lines += ['v0.1.2 over-admission was observed and excluded from equivalent-correctness comparisons.','']
        if env.get('profile',{}).get('profile')=='smoke':
            lines += ['**CI smoke: operational/correctness validation only.** Short single rounds cannot establish a performance regression or capacity limit.','']
    if env:
        profile=env.get('profile',{})
        lines += [f"Git: `{esc(env.get('git_sha','unknown'))}`; dirty: **{bool(env.get('dirty'))}**; profile: `{esc(profile.get('profile'))}`; resources: `{esc(profile.get('resources','safe'))}`.",
                  f"Resource layout: `{esc(json.dumps(env.get('resource_layout',{}),sort_keys=True))}`.",'']
        if env.get('resource_layout',{}).get('shared_cpuset'):
            lines += ['CI shares CPU sets explicitly; these measurements are not comparable to the disjoint local Safe layout.','']
    if rows:
        lines += ['## Observations','',
                  '| Variant / scenario | Target / completed RPS | 200 / 429 / errors | p99 ms | Peak / mean in flight | Assessment |',
                  '| --- | ---: | ---: | ---: | ---: | --- |']
        for r in rows[:80]:
            error=sum(r.get(k,0) for k in ('http_503','http_other','errors'))
            lines.append(f"| {esc(r.get('variant'))} / {esc(r.get('scenario'))} | {fmt(r.get('target_rps'))} / {fmt(r.get('achieved_rps'))} | {r.get('http_200','N/A')} / {r.get('http_429','N/A')} / {error} | {fmt(r.get('p99_ms'))} | {r.get('peak_in_flight','N/A')} / {fmt(r.get('mean_in_flight'))} | {esc(assess(r))} |")
        if len(rows)>80:
            lines += ['', 'Table truncated; the artifact contains every round.']
        levels=collections.defaultdict(list)
        for r in valid:
            if r.get('variant')!='governor':
                levels[(r.get('variant'),r.get('scenario','').split('/')[0])].append(r.get('target_rps',0))
        lines += ['','## Highest valid offered rates','']
        for (variant,backend),rates in sorted(levels.items()):
            lines += [f'- {esc(variant)} / {esc(backend)}: {max(rates):g} RPS validated in at least one cell; **not a capacity estimate**.']
        lines += ['', '## Exclusions and next step','']
        for r in rows:
            if r['invalid_reasons']:
                lines += ['- '+esc(r.get('variant'))+' / '+esc(r.get('scenario'))+': '+esc(', '.join(r['invalid_reasons']))]
        if any('loadgen pressure' in assess(r) for r in rows):
            lines += ['- Increase generator efficiency before increasing offered rate; generator pressure prevents attributing the limit to the server.']
        elif env.get('profile',{}).get('profile')=='explore':
            lines += ['- No boundary was demonstrated within this probe range. Confirm the highest level with longer repeats before considering any new resource/rate ceiling; the maximum remains unknown.']
        else:
            lines += ['- If no pressure was demonstrated, use a bounded local `explore` sweep; a higher rate alone does not prove saturation.']
        lines += ['- Confirm any candidate boundary with longer repeated runs, identical quotas/resources and latency/error criteria. Short-run p99 values remain descriptive.']
    lines += ['', '## Files','', '`report.md`: full tables; `raw.json`: all rounds; `validated.json`: exclusions/statistics; `environment.json`: platform/limits/images; `run.log`: build and execution log.']
    if artifact_url:
        lines += ['', f'[Download benchmark diagnostics artifact]({artifact_url})']
    else:
        lines += ['', 'On GitHub Actions, download **benchmark-diagnostics** from this run’s **Artifacts** section.']
    return '\n'.join(lines)+'\n'


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('folder',type=Path)
    p.add_argument('--status',default='unknown')
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--github-summary',action='store_true')
    a=p.parse_args()
    content=render(a.folder,a.status,os.environ.get('BENCHMARK_ARTIFACT_URL',''))
    a.output.parent.mkdir(parents=True,exist_ok=True)
    a.output.write_text(content)
    if a.github_summary and os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'],'a') as stream:
            stream.write(content[:200000])


if __name__=='__main__':
    main()
