"""Bounded open-loop HTTP/1.1 generator; no third-party dependencies."""
import collections
import concurrent.futures
import http.client
import json
import math
import os
import threading
import time
from pathlib import Path

threading.stack_size(128 * 1024)
local = threading.local()


class Activity:
    """Measure actual request overlap, independently of executor queue size."""
    def __init__(self):
        self.lock = threading.Lock()
        self.active = self.peak = 0
        self.area = 0.0
        self.last = time.monotonic()

    def change(self, delta):
        with self.lock:
            now = time.monotonic()
            self.area += self.active * (now - self.last)
            self.last = now
            self.active += delta
            self.peak = max(self.peak, self.active)


def request(client, connections=None, due=None, activity=None):
    start = time.monotonic()
    if activity:
        activity.change(1)
    try:
        if not hasattr(local, 'connection'):
            local.connection = http.client.HTTPConnection('server', 8080, timeout=3)
            if connections is not None:
                connections.append(local.connection)
        local.connection.request('GET', '/', headers={'x-bench-client': client})
        response = local.connection.getresponse()
        status = response.status
        response.read()
    except Exception:
        status = 0
        if hasattr(local, 'connection'):
            local.connection.close()
            del local.connection
    finally:
        end = time.monotonic()
        if activity:
            activity.change(-1)
    return client, status, (end-start)*1000, (start-due)*1000 if due is not None else 0, end


def percentiles(values):
    ordered = sorted(values)
    # Nearest-rank empirical percentiles. Never imply more samples than collected.
    return {f'p{p}_ms': ordered[max(0, math.ceil(len(ordered)*p/100)-1)] if ordered else None
            for p in (50, 95, 99)}


def phase(seconds, rate, concurrency, clients, prefix, capture=None):
    results, pending, connections = [], set(), []
    dropped_capacity = dropped_late = 0
    activity = Activity()
    start = time.monotonic()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
            for i in range(int(seconds*rate)):
                due = start + i/rate
                delay = due-time.monotonic()
                if delay > 0:
                    time.sleep(delay)
                done = {f for f in pending if f.done()}
                for f in done:
                    results.append(f.result())
                pending -= done
                if len(pending) >= concurrency:
                    dropped_capacity += 1
                elif time.monotonic()-due > .1:
                    dropped_late += 1
                else:
                    pending.add(pool.submit(request, f'{prefix}-{i%clients}', connections, due, activity))
            # Observe the entire offered-load interval, including the final pacing gap.
            delay = start+seconds-time.monotonic()
            if delay > 0:
                time.sleep(delay)
            for f in concurrent.futures.as_completed(pending):
                results.append(f.result())
        completed_in_window=sum(end <= start+seconds for *_,end in results)
        elapsed = max(seconds, time.monotonic()-start)
    finally:
        for connection in connections:
            connection.close()
    if capture is not None:
        capture.extend(results)
    return summarize_results(results,seconds,rate,elapsed,completed_in_window,activity,concurrency,dropped_capacity,dropped_late)


def summarize_results(results,seconds,rate,elapsed,completed_in_window,activity,concurrency,dropped_capacity=0,dropped_late=0):
    counts = collections.Counter(s for _,s,_,_,_ in results)
    admissions = collections.Counter(c for c,s,_,_,_ in results if s==200)
    attempts = collections.Counter(c for c,s,_,_,_ in results if s in (200,429))
    dropped = dropped_capacity+dropped_late
    divisor = elapsed if elapsed else 1
    return dict(schema_version=2, target_rps=rate,
                offered_rps=len(results)/seconds if seconds else 0,
                achieved_rps=len(results)/divisor,
                completed_in_window=completed_in_window,
                completion_rps_in_window=completed_in_window/seconds if seconds else 0,
                elapsed_seconds=elapsed, measurement_seconds=seconds, drain_seconds=max(0,elapsed-seconds),
                scheduled=int(seconds*rate), sample_count=len(results), dropped=dropped,
                dropped_capacity=dropped_capacity, dropped_late=dropped_late,
                concurrency_limit=concurrency, peak_in_flight=activity.peak,
                mean_in_flight=activity.area/divisor,
                **percentiles([t for _,_,t,_,_ in results]),
                scheduled_latency=percentiles([t+lag for _,_,t,lag,_ in results]),
                dispatch_lag=percentiles([lag for _,_,_,lag,_ in results]),
                status_latency={str(status):percentiles([t for _,s,t,_,_ in results if s==status])
                                for status in (200,429,503) if counts[status]},
                http_200=counts[200], http_429=counts[429], http_503=counts[503],
                http_other=sum(n for s,n in counts.items() if s not in (0,200,429,503)),
                errors=counts[0], max_admissions_per_client=max(admissions.values(),default=0),
                expected_admissions=sum(min(n,int(os.environ['LIMIT'])) for n in attempts.values()),
                clients_observed=len(set(c for c,_,_,_,_ in results)), loadgen_saturated=dropped>0)


def cgroup():
    return {name:Path('/sys/fs/cgroup',name).read_text() for name in
            ('cpu.stat','memory.current','memory.peak','memory.events','memory.stat')}


if __name__=='__main__':
    rate,concurrency,clients=(int(os.environ[k]) for k in ('RATE','CONCURRENCY','CLIENTS'))
    for _ in range(100):
        if request('readiness')[1] in (200,429):
            break
        time.sleep(.1)
    else:
        raise SystemExit('server did not become ready')
    if hasattr(local,'connection'):
        local.connection.close()
        del local.connection
    phase(float(os.environ['WARMUP']),rate,concurrency,clients,'warmup')
    before=cgroup()
    result=phase(float(os.environ['DURATION']),rate,concurrency,clients,'measure')
    result['loadgen_cgroup_before']=before
    result['loadgen_cgroup']=cgroup()
    print(json.dumps(result))
