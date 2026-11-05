"""Scoped workload verdicts; independent hosts and probe rounds never share a rank."""
import collections
import math
import statistics

import report
import capacity as search
import workloads


def passed(row):
    return not report.validate(row) and not report.load_failures(row)


def context(row):
    scenario=row.get('scenario','').rsplit('/',1)[0]
    return tuple(row.get(k) for k in ('platform_id','resource_layout_id','http_version',
                 'resource_profile','server_cpu_limit','loadgen_cpu_limit'))+(scenario,)


def qualified(items):
    return (len(items)>=5 and len({r.get('repeat') for r in items})==len(items)
            and all(r.get('measurement_seconds',0)>=30 and passed(r) for r in items))


def ratio_interval(head, peer, metric):
    """Paired log-ratio t interval; small-sample normality is an explicit assumption."""
    left={r['repeat']:r for r in head}
    right={r['repeat']:r for r in peer}
    pairs=sorted(left.keys() & right.keys())
    values=[]
    for repeat in pairs:
        a,b=left[repeat].get(metric),right[repeat].get(metric)
        if a is None or b is None or not math.isfinite(a) or not math.isfinite(b) or a<=0 or b<=0:
            return None
        values.append(math.log(b/a))
    n=len(values)
    # The formal runner caps repetitions at five. Reject unsupported counts.
    if n!=5: return None
    mean=statistics.mean(values)
    margin=2.776445105*statistics.stdev(values)/math.sqrt(n)
    return dict(ratio=math.exp(mean),low=math.exp(mean-margin),high=math.exp(mean+margin),pairs=n)


def analyze(rows):
    groups=collections.defaultdict(list)
    for row in rows:
        if row.get('target_rps',0)>0 and row.get('campaign_stage','confirm')=='confirm' and workloads.mode(row) in ('allow','mixed','reject'):
            groups[context(row)].append(row)
    capacity=[]
    costs=[]
    boundaries=[]
    for key,items in groups.items():
        cells=collections.defaultdict(list)
        for row in items: cells[(row['variant'],row['target_rps'])].append(row)
        clean=lambda rs: len(rs)==5 and len({r.get('repeat') for r in rs})==5 and all(
            r.get('measurement_seconds',0)>=30 and not report.validate(r)
            and r.get('loadgen_cpu_pct',float('inf'))<85*r.get('loadgen_cpu_limit',1) for r in rs)
        overloaded=lambda rs: clean(rs) and all(search.overloaded(r) for r in rs)
        for variant in sorted({v for v,_ in cells}):
            good=sorted(rate for (v,rate),rs in cells.items() if v==variant and qualified(rs))
            bad=sorted(rate for (v,rate),rs in cells.items() if v==variant and overloaded(rs))
            if not good and not bad: continue
            low=max(good,default=None)
            non_monotonic=low is not None and any(rate<low for rate in bad)
            high=min((rate for rate in bad if low is None or rate>low),default=None)
            boundaries.append(dict(platform_id=key[0],condition=key[-1],variant=variant,
                highest_pass=low,lowest_fail=None if non_monotonic else high,
                gap=None if non_monotonic or low is None or high is None else high-low,
                passing_levels=good,failing_levels=bad,
                state='non-monotonic' if non_monotonic else 'bracketed' if low and high else 'no confirmed upper bound' if low else 'no confirmed passing level'))
        peers=sorted({v for v,_ in cells}-{'head','baseline','v0.1.2'})
        for peer in peers:
            levels=sorted({rate for v,rate in cells if v in ('head',peer)})
            head_levels=[rate for rate in levels if qualified(cells['head',rate])]
            peer_levels=[rate for rate in levels if qualified(cells[peer,rate])]
            winner='inconclusive'
            reason='no confirmed pass/fail separation at a common tested rate'
            for rate in reversed(levels):
                h,p=cells['head',rate],cells[peer,rate]
                fields=('quota','window_seconds','measurement_seconds','warmup_seconds')
                if any(len({r.get(k) for r in h+p})!=1 for k in fields): continue
                if not workloads.matched_mix(h,p):
                    if h and p: reason='response mixes differ by more than two percentage points; comparison withheld'
                    continue
                if qualified(h) and overloaded(p): winner,reason='head',f'HEAD passed all repeats at {rate} RPS; peer failed load criteria in all repeats';break
                if qualified(p) and overloaded(h): winner,reason=peer,f'Peer passed all repeats at {rate} RPS; HEAD failed load criteria in all repeats';break
            capacity.append(dict(platform_id=key[0],condition=key[-1],competitor=peer,
                                 head_highest=max(head_levels,default=None),peer_highest=max(peer_levels,default=None),
                                 winner=winner,reason=reason))
            for rate in levels:
                h,p=cells['head',rate],cells[peer,rate]
                if not qualified(h) or not qualified(p): continue
                # Pair only identical policies/resources/durations at this offered level.
                fields=('quota','window_seconds','measurement_seconds','warmup_seconds')
                if any(len({r.get(k) for r in h+p})!=1 for k in fields): continue
                baseline=cells['baseline',rate]
                for metric in ('scheduled_p99_ms','server_cpu_pct'):
                    hs=[{**r,'scheduled_p99_ms':r.get('scheduled_latency',{}).get('p99_ms')} for r in h]
                    ps=[{**r,'scheduled_p99_ms':r.get('scheduled_latency',{}).get('p99_ms')} for r in p]
                    interval=ratio_interval(hs,ps,metric)
                    if interval is None: continue
                    noise_key='server_cpu_pct' if metric=='server_cpu_pct' else None
                    noise=[r.get(noise_key) if noise_key else r.get('scheduled_latency',{}).get('p99_ms') for r in baseline]
                    complete_noise=qualified(baseline) and all(x is not None and x>0 for x in noise)
                    threshold=max(.05,(max(noise)-min(noise))/statistics.median(noise)) if complete_noise else None
                    aligned=metric!='server_cpu_pct' or all(r.get('resource_phase')=='measurement'
                        and r.get('resource_capture_skew_seconds',float('inf'))<=.02*r.get('elapsed_seconds',0) for r in h+p+baseline)
                    outcome='inconclusive'
                    matched_mix=workloads.matched_mix(h,p)
                    if threshold is not None and aligned and matched_mix:
                        if interval['low']>1+threshold: outcome='head'
                        elif interval['high']<1/(1+threshold): outcome=peer
                    costs.append(dict(platform_id=key[0],condition=key[-1],rate=rate,competitor=peer,
                                      metric=metric,noise_threshold=threshold,phase_aligned=aligned,response_mix_matched=matched_mix,
                                      winner=outcome,**interval))
    return dict(capacity=capacity,costs=costs,boundaries=boundaries)


def markdown(data):
    if not data['capacity'] and not data.get('boundaries'): return ''
    esc=lambda x:str(x).replace('|','/').replace('\n',' ')
    fmt=lambda x:'N/A' if x is None else f'{x:.3f}'
    lines=['## Confirmed passing and failing bounds','',
           'Only five clean repeats of at least 30 seconds establish a bound. Brackets locate tested endpoints; they do not prove every untested intermediate rate. Unbounded, non-monotonic and unresolved cases remain explicit.','',
           '| Host / condition | Variant | Highest pass RPS | Lowest fail RPS | Gap RPS | State |',
           '| --- | --- | ---: | ---: | ---: | --- |']
    for b in data.get('boundaries',[]):
        lines.append('| '+esc(str(b['platform_id'])+' / '+b['condition'])+' | '+esc(b['variant'])+' | '+str(b['highest_pass'])+' | '+str(b['lowest_fail'])+' | '+str(b['gap'])+' | '+b['state']+' |')
    lines += ['', '## Workload verdicts','',
           'Each verdict is scoped to one recorded host, backend and workload. Different algorithms retain different guarantees. A sustained-load winner passed a tested rate where the peer failed all five clean confirmation repeats; this is not an unbounded maximum-throughput claim.',
           'Five repeats of at least 30 seconds are required. Mixed outcomes, invalid measurements and generator CPU pressure cannot establish a sustained-load winner.','',
           '| Host / condition | Peer | HEAD / peer highest qualified RPS | Sustained-load winner | Evidence |',
           '| --- | --- | ---: | --- | --- |']
    for c in data['capacity']:
        lines.append('| '+esc(str(c['platform_id'])+' / '+c['condition'])+' | '+esc(c['competitor'])+' | '+str(c['head_highest'])+' / '+str(c['peer_highest'])+' | '+esc(c['winner'])+' | '+esc(c['reason'])+' |')
    lines += ['', '### Cost at jointly qualified rates','',
              'Ratios are peer / HEAD, paired by repeat. Intervals are approximate 95% t intervals on five log ratios, assuming approximately normal independent repeat-block effects. They are exploratory per-comparison intervals, not simultaneous confidence guarantees or a global ranking.',
              'A directional result requires the whole interval to exceed the larger of a 5% practical margin and baseline A/A relative range. Rejection fractions must match within two percentage points. Missing calibration, unaligned CPU counters or CPU capture skew above 2% yield inconclusive. RSS snapshots are not ranked.','',
              '| Host / condition | Peer | RPS | Metric | Ratio [95% interval] | A/A + practical margin | Matched response mix | Winner |',
              '| --- | --- | ---: | --- | ---: | ---: | --- | --- |']
    for c in data['costs']:
        lines.append('| '+esc(str(c['platform_id'])+' / '+c['condition'])+' | '+esc(c['competitor'])+' | '+str(c['rate'])+' | '+c['metric']+' | '+fmt(c['ratio'])+' ['+fmt(c['low'])+', '+fmt(c['high'])+'] | '+fmt(c['noise_threshold'])+' | '+str(c.get('response_mix_matched','unknown'))+' | '+esc(c['winner'])+' |')
    return '\n'.join(lines)+'\n'
