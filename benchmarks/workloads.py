"""Representative workloads, positive rejection quotas and observable coverage."""
import itertools
import statistics
import collections

MODES=('allow','mixed','reject')


def cases(full=False):
    pairs=itertools.product((1,100,10000),MODES) if full else (
        (1,'allow'),(100,'allow'),(10000,'allow'),(100,'mixed'),(100,'reject'))
    return [dict(clients=clients,mode=mode) for clients,mode in pairs]


def quota(mode, rate, seconds, warmup, clients):
    if mode=='allow': return rate*(seconds+warmup)+100
    if mode=='deny': return 0
    fraction=.01 if mode=='reject' else .5
    return max(1,int(rate*seconds/clients*fraction))


def mode(row):
    parts=row.get('scenario','').split('/')
    return row.get('workload_mode',parts[3] if len(parts)>3 else 'allow')


def rejection_fraction(row):
    total=row.get('http_200',0)+row.get('http_429',0)
    return row.get('http_429',0)/total if total else None


def coverage_failures(row):
    reasons=[]
    clients=row.get('workload_clients')
    if clients is not None and row.get('clients_observed',0)<clients:
        reasons.append('client cardinality not exercised')
    if row.get('variant')=='baseline': return reasons
    fraction=rejection_fraction(row)
    if mode(row)=='allow' and row.get('http_429',0)>0:
        reasons.append('unexpected rejection in all-allow workload')
    if mode(row)=='mixed' and (fraction is None or not .2<=fraction<=.8):
        reasons.append('mixed rejection fraction outside 20%-80%')
    if mode(row)=='reject' and (fraction is None or fraction<.9):
        reasons.append('majority-reject fraction below 90%')
    return reasons


def matched_mix(head, peer):
    """Different rejection ratios change the amount of work performed by the SUT."""
    if not head or not peer: return False
    h=[rejection_fraction(r) for r in head];p=[rejection_fraction(r) for r in peer]
    return (all(x is not None for x in h+p)
            and abs(statistics.median(h)-statistics.median(p))<=.02)


def markdown(rows):
    groups=collections.defaultdict(list)
    for row in rows:
        if 'workload_clients' in row:
            groups[(row.get('platform_id'),row.get('campaign_stage','fixed'),row.get('variant'),row.get('scenario'))].append(row)
    if not groups: return ''
    esc=lambda value:str(value).replace('|','/').replace('\n',' ')
    lines=['## Workload coverage and quota observations','',
           'Ratios are observed, not assumed. Baseline always allows. Only rows labeled first-window burst oracle assert exact admitted counts; other policies retain different semantics. This does not prove rolling-window correctness.','',
           '| Host / stage / variant / scenario | Clients requested / observed min | 200 / 429 total | Rejection fraction range | Exact expected admissions | Coverage |',
           '| --- | ---: | ---: | ---: | ---: | --- |']
    for key,items in sorted(groups.items(),key=lambda pair:str(pair[0])):
        fractions=[rejection_fraction(r) for r in items if rejection_fraction(r) is not None]
        ratio=f'{min(fractions):.1%}–{max(fractions):.1%}' if fractions else 'N/A'
        expected=str(sum(r.get('expected_admissions',0) for r in items)) if all(r.get('correctness')=='first-window burst oracle' for r in items) else 'not asserted for this policy'
        failures=sorted({reason for r in items for reason in coverage_failures(r)})
        requested=max(r['workload_clients'] for r in items);observed=min(r.get('clients_observed',0) for r in items)
        lines.append('| '+esc(' / '.join(str(x) for x in key))+' | '+str(requested)+' / '+str(observed)+' | '+str(sum(r.get('http_200',0) for r in items))+' / '+str(sum(r.get('http_429',0) for r in items))+' | '+ratio+' | '+expected+' | '+esc('; '.join(failures) or 'exercised; inspect correctness/errors separately')+' |')
    return '\n'.join(lines)+'\n'
