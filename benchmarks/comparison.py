"""Descriptive cost contrasts at matched workloads, never an algorithm winner."""
import collections
import statistics


def contrasts(rows):
    groups=collections.defaultdict(list)
    for row in rows:
        if row.get('workload')=='behavior' or row.get('invalid_reasons') or row.get('dropped',0)>0:
            continue
        key=tuple(row.get(k) for k in ('platform_id','scenario','target_rps','measurement_seconds',
                                      'warmup_seconds','http_version','resource_layout_id','resource_profile','server_cpu_limit','loadgen_cpu_limit','quota','window_seconds'))
        groups[key].append(row)
    result=[]
    for group in groups.values():
        head=[r for r in group if r['variant']=='head']
        if not head:
            continue
        for name in sorted({r['variant'] for r in group}-{'head','baseline','v0.1.2'}):
            peer=[r for r in group if r['variant']==name]
            med=lambda rs,k: statistics.median(r[k] for r in rs) if all(r.get(k) is not None for r in rs) else None
            ratio=lambda k: med(peer,k)/med(head,k) if med(head,k) and med(peer,k) is not None else None
            enough=min(len(head),len(peer))>=5
            separation=enough and (max(r['p99_ms'] for r in head)<min(r['p99_ms'] for r in peer) or max(r['p99_ms'] for r in peer)<min(r['p99_ms'] for r in head))
            result.append(dict(variant=name,algorithm=peer[0].get('algorithm','unknown'),scenario=peer[0]['scenario'],
                               repeats=min(len(head),len(peer)),p99_ratio=ratio('p99_ms'),cpu_ratio=ratio('server_cpu_pct'),
                               rss_ratio=ratio('server_rss_bytes'),redis_ratio=ratio('redis_memory_bytes'),
                               evidence='non-overlapping observed p99 ranges; descriptive only' if separation else ('overlapping p99 ranges' if enough else 'fewer than five matched repeats')))
    return result
