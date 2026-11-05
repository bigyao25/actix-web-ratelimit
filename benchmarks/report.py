#!/usr/bin/env python3
"""Validate externally collected benchmark rounds and summarize valid results."""
import argparse
import json
import math
import statistics
import workloads
from pathlib import Path


def validate(round_):
    reasons = []
    required = ("platform_id", "variant", "scenario", "offered_rps", "achieved_rps",
                "p50_ms", "p95_ms", "p99_ms", "http_200", "http_429", "http_503",
                "http_other", "errors", "oom", "loadgen_saturated", "redis_oom",
                "cpu_throttled", "memory_peak_bytes", "quota_exceeded")
    if round_.get('correctness_mismatch'):
        reasons.append('correctness_mismatch')
    missing = [key for key in required if key not in round_ or round_[key] is None]
    if missing:
        reasons.append("missing: " + ", ".join(missing))
        return reasons
    for key in ("offered_rps", "achieved_rps", "p50_ms", "p95_ms", "p99_ms",
                "http_200", "http_429", "http_503", "http_other", "errors", "memory_peak_bytes"):
        value = round_[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
            reasons.append("invalid " + key)
    for key in ("oom", "loadgen_saturated", "redis_oom", "cpu_throttled", "quota_exceeded"):
        if not isinstance(round_[key], bool):
            reasons.append("invalid " + key)
        elif round_[key]:
            if key!='loadgen_saturated' or round_.get('schema_version',0)<3:
                reasons.append(key)
    for key in ("http_503", "http_other", "errors"):
        if round_[key]:
            reasons.append(key)
    if round_.get('schema_version',0)>=3 and round_.get('dropped_late',0)>0:
        reasons.append('scheduler_drops')
    if any(reason.startswith('invalid ') for reason in reasons):
        return reasons
    if not reasons and not round_["p50_ms"] <= round_["p95_ms"] <= round_["p99_ms"]:
        reasons.append("inconsistent latency percentiles")
    if round_.get('schema_version',0)>=2:
        total=sum(round_[k] for k in ('http_200','http_429','http_503','http_other','errors'))
        if total != round_.get('sample_count') or total+round_.get('dropped',0) != round_.get('scheduled'):
            reasons.append('request accounting mismatch')
        if round_['achieved_rps'] > round_['offered_rps']+1e-6:
            reasons.append('completion RPS exceeds offered RPS')
        if not 0 <= round_.get('mean_in_flight',-1) <= round_.get('peak_in_flight',-1) <= round_.get('concurrency_limit',-1):
            reasons.append('invalid measured concurrency')
    if round_.get('schema_version',0)>=3:
        categories=round_.get('error_categories')
        if not isinstance(categories,dict) or any(not isinstance(v,int) or isinstance(v,bool) or v<0 for v in categories.values()):
            reasons.append('missing/invalid error categories')
        elif sum(categories.values())!=round_['errors']:
            reasons.append('error category accounting mismatch')
        if round_.get('dropped_capacity',0)+round_.get('dropped_late',0)!=round_.get('dropped'):
            reasons.append('drop accounting mismatch')
        if round_.get('loadgen_saturated') != (round_.get('dropped',0)>0):
            reasons.append('drop flag mismatch')
    return reasons


def invalid_comparison_baseline(row):
    return row.get('workload') in ('comparison','pk') and row.get('variant')=='baseline' and bool(validate(row))


def load_failures(row):
    reasons=[]
    if row.get('dropped_capacity',0)>0: reasons.append('in-flight limit reached')
    if row.get('workload') in ('comparison','pk'):
        reasons += workloads.coverage_failures(row)
        if row.get('http_rps',row.get('achieved_rps',0))<.99*row.get('target_rps',1): reasons.append('HTTP completion below 99%')
        if (row.get('scheduled_latency',{}).get('p99_ms') or float('inf'))>10: reasons.append('scheduled p99 above 10ms')
        if row.get('loadgen_cpu_pct',float('inf'))>=85*row.get('loadgen_cpu_limit',1): reasons.append('loadgen CPU pressure')
    return reasons


def summarize(data):
    groups = {}
    annotated = []
    for item in data["rounds"]:
        reasons = validate(item)
        annotated.append({**item, "invalid_reasons": reasons, "load_failure_reasons": load_failures(item)})
        if not reasons and not load_failures(item):
            key = (item["platform_id"], item["variant"], item["scenario"], item.get("target_rps"), item.get("measurement_seconds"), item.get("warmup_seconds"))
            groups.setdefault(key, []).append(item)
    rows = []
    for (platform, variant, scenario, rate, duration, warmup), items in sorted(groups.items(), key=lambda pair: str(pair[0])):
        rps = [item["achieved_rps"] for item in items]
        rows.append({"platform_id": platform, "variant": variant, "scenario": scenario,
                     "target_rps": rate, "measurement_seconds": duration, "warmup_seconds": warmup,
                     "p99_min_ms": min(item["p99_ms"] for item in items),
                     "p99_max_ms": max(item["p99_ms"] for item in items),
                     "min_samples": min(item.get("sample_count",sum(item[k] for k in ("http_200","http_429","http_503","http_other","errors"))) for item in items),
                     "legacy_timing": any(item.get("schema_version",1)<2 for item in items),
                     "rounds": len(items), "median_rps": statistics.median(rps),
                     "min_rps": min(rps), "max_rps": max(rps),
                     "median_p99_ms": statistics.median(item["p99_ms"] for item in items)})
    for row in rows:
        same = [r for r in rows if r['platform_id']==row['platform_id'] and r['scenario']==row['scenario'] and all(r[k]==row[k] for k in ('target_rps','measurement_seconds','warmup_seconds'))]
        for variant, label in [('baseline','baseline'),('v0.2.1','reference')]:
            ref = next((r for r in same if r['variant']==variant), None) if row['variant'] not in ('governor','trypema','extensible') and '/behavior/' not in row['scenario'] else None
            row[label+'_rps_change_pct'] = (100*(row['median_rps']/ref['median_rps']-1) if ref and ref['median_rps'] else None)
            row[label+'_p99_overhead_ms'] = row['median_p99_ms']-ref['median_p99_ms'] if ref else None
        row['aa_spread_pct'] = 100*(row['max_rps']-row['min_rps'])/row['median_rps'] if row['median_rps'] and row['rounds']>1 else None
        row['warnings']=[]
        if row['rounds']<5: row['warnings'].append('fewer than five repeats')
        if row['measurement_seconds'] is None or row['measurement_seconds']<30: row['warnings'].append('short/unknown measurement duration')
        if row['min_samples']<10000: row['warnings'].append('p99 has fewer than 10000 samples per round')
        if row['legacy_timing']: row['warnings'].append('legacy RPS timing; completion rate may exceed target due to boundary bias')
    return {"rounds": annotated, "summary": rows}


def markdown(result):
    lines = ["# Benchmark report", "", "Only measurement-clean rounds passing their load criteria are summarized; clean overload observations remain in raw tables.", "",
             "| Platform | Variant | Scenario | Rounds | Median RPS | Range RPS | Median p99 ms |",
             "| --- | --- | --- | ---: | ---: | ---: | ---: |"]
    esc = lambda s: str(s).replace("|", "\\|").replace("\n", " ")
    for row in result["summary"]:
        lines.append(f"| {esc(row['platform_id'])} | {esc(row['variant'])} | {esc(row['scenario'])} | {row['rounds']} | {row['median_rps']:.2f} | {row['min_rps']:.2f}–{row['max_rps']:.2f} | {row['median_p99_ms']:.2f} |")
    invalid = [r for r in result["rounds"] if r["invalid_reasons"]]
    lines += ["", f"Excluded rounds: {len(invalid)}", ""]
    for r in invalid:
        lines.append("- " + str(r.get("variant", "unknown")).replace("\n", " ") + ": " + ", ".join(r["invalid_reasons"]))
    lines += ['', '## Comparisons', '', 'RPS is achieved at a capped offered rate, not maximum throughput. GCRA comparisons are intentionally omitted because semantics differ.',
              'Repeat RPS spread describes paced completions, not platform capacity noise or a confidence interval. A single round is N/A.', '',
              '| Variant / scenario | Completion RPS vs baseline % | Completion RPS vs v0.2.1 % | p99 overhead vs baseline ms | Repeat RPS spread % |',
              '| --- | ---: | ---: | ---: | ---: |']
    def fmt(v): return 'N/A' if v is None else f'{v:.2f}'
    for r in result['summary']:
        lines.append('| '+esc(r['variant']+' / '+r['scenario'])+' | '+' | '.join(fmt(r[k]) for k in ('baseline_rps_change_pct','reference_rps_change_pct','baseline_p99_overhead_ms','aa_spread_pct'))+' |')
    lines += ['', '## Raw rounds (including invalid runs)', '', '| Variant | Scenario | Target RPS | Offered RPS | Achieved RPS | 200 | 429 | 503 | Valid |', '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
    for r in result['rounds']:
        lines.append('| '+' | '.join(str(r.get(k,'N/A')).replace('|','/').replace('\n',' ') for k in ('variant','scenario','target_rps','offered_rps','achieved_rps','http_200','http_429','http_503'))+' | '+str(not r['invalid_reasons'])+' |')
    lines += ['', '## Resource diagnostics', '', '| Variant / scenario | Server CPU % (100 = one core) | Server RSS bytes | Server peak bytes | Loadgen peak bytes | Redis bytes | Redis connections | Resource phase | Capture skew seconds |', '| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: |']
    for r in result['rounds']:
        lines.append('| '+str(r.get('variant','unknown'))+' / '+str(r.get('scenario','unknown'))+' | '+' | '.join(str(r.get(k,'N/A')) for k in ('server_cpu_pct','server_rss_bytes','memory_peak_bytes','loadgen_peak_bytes','redis_memory_bytes','redis_connections','resource_phase','resource_capture_skew_seconds'))+' |')
    lines += ['', '## Measurement quality', '',
              'Latency percentiles describe sent requests; scheduled latency also includes dispatch lag. Dropped arrivals are counted separately.',
              'Native compare brackets server/Redis CPU reads around measurement with paused traffic; capture skew is recorded. Other/legacy CPU samples include warmup and sampling overhead. Loadgen CPU deltas cover measurement only.',
              'Legacy report RSS may be anonymous-memory estimates. See schema_version and the recorded collector source.', '',
              '| Variant / scenario | Samples per round (min) | p99 range ms | Warnings |',
              '| --- | ---: | --- | --- |']
    for r in result['summary']:
        lines.append('| '+esc(r['variant']+' / '+r['scenario'])+' | '+str(r['min_samples'])+' | '+fmt(r['p99_min_ms'])+'–'+fmt(r['p99_max_ms'])+' | '+esc('; '.join(r['warnings']) or 'none')+' |')
    lines += ['', '## Offered load and actual concurrency', '',
              '| Variant / scenario | Target RPS | Offered RPS | Completion RPS | Peak / mean in flight | Dispatch p99 ms | Scheduled p99 ms | Loadgen CPU % |',
              '| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: |']
    for r in result['rounds']:
        lines.append('| '+esc(str(r.get('variant'))+' / '+str(r.get('scenario')))+' | '+' | '.join(fmt(r.get(k)) for k in ('target_rps','offered_rps','achieved_rps'))+' | '+str(r.get('peak_in_flight','N/A'))+' / '+fmt(r.get('mean_in_flight'))+' | '+fmt(r.get('dispatch_lag',{}).get('p99_ms'))+' | '+fmt(r.get('scheduled_latency',{}).get('p99_ms'))+' | '+fmt(r.get('loadgen_cpu_pct'))+' |')
    lines += ['', '## Latency by response status', '', '| Variant / scenario | Status | p50 ms | p95 ms | p99 ms |', '| --- | ---: | ---: | ---: | ---: |']
    for r in result['rounds']:
        for status, values in r.get('status_latency',{}).items():
            lines.append('| '+esc(str(r.get('variant'))+' / '+str(r.get('scenario')))+' | '+status+' | '+' | '.join(fmt(values.get(k)) for k in ('p50_ms','p95_ms','p99_ms'))+' |')
    lines += ['', workloads.markdown(result['rounds']), '## Measurement quality versus load outcome', '',
              '| Variant / scenario | Measurement | Load outcome | HTTP RPS | Transport categories | Capacity / scheduler drops |',
              '| --- | --- | --- | ---: | --- | ---: |']
    for r in result['rounds']:
        quality='invalid: '+', '.join(r['invalid_reasons']) if r['invalid_reasons'] else 'clean'
        load=', '.join(r.get('load_failure_reasons',[])) or 'criteria met / not applicable'
        lines.append('| '+esc(str(r.get('variant'))+' / '+str(r.get('scenario')))+' | '+esc(quality)+' | '+esc(load)+' | '+fmt(r.get('http_rps'))+' | '+esc(str(r.get('error_categories','legacy: unknown')))+' | '+str(r.get('dropped_capacity','N/A'))+' / '+str(r.get('dropped_late','N/A'))+' |')
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--markdown", type=Path, required=True)
    args = parser.parse_args()
    result = summarize(json.loads(args.input.read_text()))
    args.json.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n")
    args.markdown.write_text(markdown(result))


if __name__ == "__main__":
    main()
