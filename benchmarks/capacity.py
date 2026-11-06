"""Bounded capacity refinement; invalid measurements never become failing bounds."""
import report

RANGES={'memory':[45000,60000],'redis':[10000,16000]}
RESOLUTION={'memory':5000,'redis':2000}
CAPACITY_FAILURES={'in-flight limit reached','HTTP completion below 99%','scheduled p99 above 10ms'}
# A generator at its own CPU quota cannot attribute a limit to the server, and its
# late drops invalidate the round. Keep the top probe below this fraction of quota.
LOADGEN_HEADROOM=.70
LOADGEN_STEPS=2


def passed(row):
    return not report.validate(row) and not report.load_failures(row)


def loadgen_fraction(row):
    """Generator CPU as a fraction of its own quota (loadgen_cpu_pct is a percentage)."""
    return row.get('loadgen_cpu_pct',0)/(100*(row.get('loadgen_cpu_limit') or 1))


def generator_limited(row):
    """True when the only invalidity is the generator missing its own schedule."""
    reasons=set(report.validate(row))
    return not reasons or reasons=={'scheduler_drops'}


def ladder_step(rates, rows, resolution):
    """Scale the probe ladder down when the generator cannot sustain its top rate.

    Returns None when the generator keeps headroom, so a healthy host keeps the
    configured ladder unchanged. The caller probes the top rate first; a saturated
    generator is a harness limit, never a server capacity finding.
    """
    if not rates or rates[-1]<=0: return None
    baseline=[r for r in rows if r.get('variant')=='baseline' and generator_limited(r)]
    if not baseline: return None
    row=max(baseline,key=lambda r:r.get('target_rps',0))
    used=loadgen_fraction(row)
    late=row.get('dropped_late',0)
    if not late and used<LOADGEN_HEADROOM: return None
    achieved=row.get('achieved_rps') or row.get('target_rps') or 0
    if used<=0 or achieved<=0: return None
    # CPU per achieved RPS; late drops mean the generator already missed arrivals.
    per_rps=used/achieved
    top=max(1000,min(100000,round(LOADGEN_HEADROOM/per_rps/1000)*1000))
    if top>=rates[-1]: top=max(1000,rates[-1]-resolution)
    scaled=sorted({max(1000,min(100000,round(rate*top/rates[-1]/500)*500)) for rate in rates})
    if scaled==sorted(rates): return None
    return dict(rate=rates[-1],loadgen_cpu_fraction=round(used,3),dropped_late=late,
                cpu_per_achieved_rps=per_rps,rates=scaled,
                reason='generator headroom below target; ladder scaled down')


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
