"""Bounded capacity refinement; invalid measurements never become failing bounds."""
import report

RANGES={'memory':[45000,60000],'redis':[10000,16000]}
RESOLUTION={'memory':5000,'redis':2000}
CAPACITY_FAILURES={'in-flight limit reached','HTTP completion below 99%','scheduled p99 above 10ms'}


def passed(row):
    return not report.validate(row) and not report.load_failures(row)


def overloaded(row):
    failures=set(report.load_failures(row))
    return not report.validate(row) and bool(failures) and failures<=CAPACITY_FAILURES


def probe_bounds(rows, peers):
    baseline={r['target_rps'] for r in rows if r['variant']=='baseline' and passed(r)}
    result={}
    for peer in peers:
        own=[r for r in rows if r['variant']==peer]
        good=sorted({r['target_rps'] for r in own if passed(r) and (peer=='baseline' or r['target_rps'] in baseline)})
        bad=sorted({r['target_rps'] for r in own if overloaded(r) and (peer=='baseline' or r['target_rps'] in baseline)})
        lower=max(good,default=None)
        non_monotonic=lower is not None and any(rate<lower for rate in bad)
        upper=min((rate for rate in bad if lower is None or rate>lower),default=None)
        result[peer]=dict(lower=lower,upper=upper,passing=good,failing=bad,non_monotonic=non_monotonic,
                          state='non-monotonic' if non_monotonic else 'bracketed' if lower and upper else 'below-tested-range' if upper else 'above-tested-range' if lower else 'unresolved')
    return result


def next_probe(rows, peers, resolution):
    bounds=probe_bounds(rows,peers)
    if bounds.get('head',{}).get('non_monotonic'): return None
    seen={r['target_rps'] for r in rows}
    for peer in ['head']+[v for v in peers if v not in ('baseline','head')]:
        b=bounds[peer];lo,hi=b['lower'],b['upper']
        if b['non_monotonic']: continue
        if lo and hi:
            if hi-lo<=resolution: continue
            rate=round((lo+hi)/1000)*500;reason='bisect'
        elif hi:
            rate=max(1000,round(hi/1000)*500);reason='lower range'
        elif lo and lo<100000:
            rate=min(100000,round(lo*1.25/500)*500);reason='extend upper range'
        else: continue
        if rate not in seen:
            return dict(rate=rate,for_variant=peer,reason=reason,previous_bounds=b)
    return None


def search_status(rows, peers, resolution, used, budget, check=False):
    bounds=probe_bounds(rows,peers)
    head=bounds.get('head',{})
    low,high=head.get('lower'),head.get('upper')
    gap=high-low if low and high and not head.get('non_monotonic') else None
    unresolved=[v for v,b in bounds.items() if v!='baseline' and (b['non_monotonic']
        or b['lower'] is None or b['upper'] is None or b['upper']-b['lower']>resolution)]
    return dict(target_gap_rps=resolution,extra_probes_used=used,extra_probe_budget=budget,
                head_probe_gap_rps=gap,head_probe_resolution_met=gap is not None and gap<=resolution,
                budget_exhausted=not check and used>=budget and next_probe(rows,peers,resolution) is not None,
                unresolved_probe_bounds=unresolved,
                evidence='short probes only; confirmed bounds require five long repeats')
