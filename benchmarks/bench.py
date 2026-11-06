#!/usr/bin/env python3
"""Sequential, resource-limited Docker Compose benchmark runner."""
import argparse
import fcntl
import hashlib
import itertools
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import tempfile
import time
import uuid
import report
import resources
import summary as assessment
import variants
import workloads

ROOT = Path(__file__).resolve().parents[1]
HERE = ROOT / 'benchmarks'
VARIANTS = tuple(variants.VARIANTS)

def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()

def docker(*args, **kwargs):
    return run('docker', *args, **kwargs)

def cgroup(container):
    script = 'cat /sys/fs/cgroup/cpu.stat; echo MEMORY; cat /sys/fs/cgroup/memory.current /sys/fs/cgroup/memory.peak; echo EVENTS; cat /sys/fs/cgroup/memory.events; echo RSS; cat /sys/fs/cgroup/memory.stat; echo SOCKETS; cat /proc/net/sockstat; echo PROCESS; cat /proc/1/status'
    return docker('exec', container, 'sh','-c',script)

def counters(raw):
    return {line.split()[0]: int(line.split()[1]) for line in raw.splitlines() if len(line.split())==2 and line.split()[1].isdigit()}

def stats_samples(raw):
    """Remove Docker's terminal controls while rejecting unexpected sampler output."""
    clean=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',raw)
    samples=[line.strip() for line in clean.splitlines() if line.strip()]
    if not samples:
        raise RuntimeError('Docker stats produced no samples')
    for line in samples:
        value=json.loads(line)
        if not isinstance(value,dict) or 'CPUPerc' not in value or 'MemUsage' not in value:
            raise RuntimeError('Unexpected Docker stats sample')
    return samples

def controlled_measurement(process, capture):
    """Bracket the native measurement with resource reads while traffic is paused."""
    before=after=None
    started=ended=None
    result=None
    for line in process.stdout:
        value=json.loads(line)
        event=value.get('event')
        if event=='measurement_ready' and before is None:
            started=time.monotonic()
            before=capture()
        elif event=='measurement_complete' and before is not None and after is None:
            after=capture()
            ended=time.monotonic()
        elif event is None and after is not None and result is None:
            result=value
            continue
        else:
            raise RuntimeError('Unexpected measurement phase event')
        process.stdin.write(event+'\n');process.stdin.flush()
    process.wait()
    if process.returncode or result is None or before is None or after is None:
        raise RuntimeError('Incomplete native measurement handshake')
    return result,before,after,ended-started

def round_diagnostic(result, rate):
    """One-line per-round diagnostics so a failed rate is explainable from run.log."""
    http=result.get('http_rps',result.get('achieved_rps',0))
    sched=result.get('scheduled_latency',{}).get('p99_ms')
    return (f"  -> http={http:.0f}/{result.get('target_rps',rate)} "
            f"p99={result.get('p99_ms',float('nan')):.2f}ms "
            f"sched_p99={'n/a' if sched is None else f'{sched:.2f}ms'} "
            f"loadgen_cpu={result.get('loadgen_cpu_pct',0):.1f}%/{result.get('loadgen_cpu_limit',1)} "
            f"server_cpu={result.get('server_cpu_pct',0):.1f}%/{result.get('server_cpu_limit',1)} "
            f"drop_capacity={result.get('dropped_capacity',0)} drop_late={result.get('dropped_late',0)} "
            f"errors={result.get('errors',0)}")

def generator_pressure(result):
    """Late drops or a generator near its own quota cannot qualify a rate.

    loadgen_cpu_pct is a percentage of one CPU, so the quota threshold is 85*limit.
    """
    return (result.get('dropped_late',0)>0
            or result.get('loadgen_cpu_pct',0)>=85*(result.get('loadgen_cpu_limit') or 1))

def verify_reuse(metadata, source, images):
    """Accept only unchanged source and the exact already-built native images."""
    keys=('git_sha','source_hashes','rust','build','http_keep_alive_seconds')
    if any(metadata.get(key)!=source.get(key) for key in keys):
        raise RuntimeError('Build reuse refused: source or build settings changed')
    for name,value in images.items():
        old=source.get('images',{}).get(name,{})
        if not old.get('Id') or any(value.get(k)!=old.get(k) for k in ('Id','Architecture','Os')):
            raise RuntimeError('Build reuse refused: image changed or missing: '+name)

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('profile', nargs='?', default='safe', choices=['safe','smoke','explore','compare','defect'])
    p.add_argument('--resources', choices=['safe','ci','comparison'], default=None)
    p.add_argument('--variants', nargs='+', choices=VARIANTS, default=None)
    p.add_argument('--backends', nargs='+', choices=['memory','redis'], default=None)
    p.add_argument('--clients', nargs='+', type=int, default=None)
    p.add_argument('--concurrency', nargs='+', type=int, default=None)
    p.add_argument('--modes', nargs='+', choices=['allow','deny','mixed','reject'], default=None)
    p.add_argument('--rate', type=int, default=None)
    p.add_argument('--memory-rates', nargs='+', type=int, help='Backend-specific rates; --rates overrides both backends')
    p.add_argument('--redis-rates', nargs='+', type=int)
    p.add_argument('--rates', nargs='+', type=int, help='Sequential rates: safe <=2000, explore <=10000, high-load comparison <=100000')
    p.add_argument('--plan', action='store_true', help='Print matrix size and load duration without Docker or builds')
    p.add_argument('--duration', type=int, default=None)
    p.add_argument('--warmup', type=int, default=None)
    p.add_argument('--repeats', type=int, default=None)
    p.add_argument('--allow-dirty', action='store_true')
    p.add_argument('--skip-build', action='store_true')
    p.add_argument('--reuse-build-manifest',type=str,help='Reuse images only after verifying recorded source, build settings and image IDs')
    p.add_argument('--output', type=Path, default=HERE/'results'/time.strftime('%Y%m%d-%H%M%S'))
    a = p.parse_args()
    explore=a.profile=='explore'
    compare=a.profile=='compare'
    defect=a.profile=='defect'
    a.backends=a.backends or (['memory'] if defect else ['memory','redis'])
    if defect:
        a.variants=['v0.1.2'];a.clients=[1];a.concurrency=[1];a.modes=['mixed']
        a.duration,a.warmup,a.repeats,a.rate=1,0,1,6
        a.rates=[6]
    elif a.variants and 'v0.1.2' in a.variants:
        p.error('Use the one-shot defect profile for v0.1.2; it is excluded from performance matrices')
    a.resources=a.resources or ('comparison' if compare else 'safe')
    if compare:
        a.variants=a.variants or ['baseline','head','governor','trypema','extensible']
        a.clients=a.clients or [1]
        a.concurrency=a.concurrency or [512]
        a.modes=a.modes or ['allow']
        a.duration=a.duration if a.duration is not None else 30
        a.warmup=a.warmup if a.warmup is not None else 5
        a.repeats=a.repeats if a.repeats is not None else 5
        if a.rates is None and a.rate is None:
            a.memory_rates=a.memory_rates or [30000,60000]
            a.redis_rates=a.redis_rates or [15000,20000,25000]
    if compare != (a.resources=='comparison'):
        p.error('high-load comparison runs require --resources comparison; this layout is exclusive to high-load comparison')
    a.variants=a.variants or (['baseline','head'] if explore else ['baseline','head','governor','trypema','extensible'] if a.profile=='smoke' else ['baseline','v0.2.1','head'])
    a.clients=a.clients or ([1] if explore else [1,100,10000])
    a.concurrency=a.concurrency or ([128] if explore else [32,128,512])
    a.modes=a.modes or (['allow'] if explore else ['allow','deny','mixed'])
    a.variants=[v for v in a.variants if any(variants.supports(v,b,m) for b,m in itertools.product(a.backends,a.modes))]
    if not a.variants: p.error('No selected variants support the requested workload')
    a.duration=a.duration if a.duration is not None else (10 if explore else 60)
    a.warmup=a.warmup if a.warmup is not None else (2 if explore else 15)
    a.repeats=a.repeats if a.repeats is not None else (1 if explore else 5)
    if explore and a.rates is None and a.rate is None:
        a.rates=[1000,2000,4000,8000,10000]
    a.rate=a.rate if a.rate is not None else 1000
    if a.resources=='ci' and a.profile not in ('smoke','defect'):
        p.error('CI resources support smoke/defect only; use safe resources for local exploration')
    if a.profile=='smoke':
        a.duration,a.warmup,a.repeats,a.rate=2,1,1,min(a.rate,500)
        a.clients,a.concurrency,a.modes=[1],[32],['mixed']
    rates_by_backend={b:a.rates or (a.memory_rates if b=='memory' else a.redis_rates) or [a.rate] for b in a.backends}
    rates=sorted({rate for values in rates_by_backend.values() for rate in values})
    if a.profile=='smoke':
        rates=[min(rate,500) for rate in rates]
    max_rate=100000 if compare else 10000 if explore else 2000
    rates=list(dict.fromkeys(rates))
    if a.profile=='smoke':
        rates_by_backend={b:list(dict.fromkeys(min(r,500) for r in rs)) for b,rs in rates_by_backend.items()}
    workload_axes=sorted({(rate,b) for b,rs in rates_by_backend.items() for rate in rs})
    if any(not 1<=rate<=max_rate for rate in rates):
        p.error(f'Every rate step must be 1..{max_rate} RPS')
    if compare and (a.duration>60 or a.warmup>15 or a.repeats>5):
        p.error('high-load comparison bounds: duration <=60, warmup <=15, repeats <=5')
    if explore and (a.duration>30 or a.warmup>10 or a.repeats>3):
        p.error('Explore bounds: duration <=30, warmup <=10, repeats <=3')
    if not (1<=a.rate<=max_rate and 1<=a.duration<=120 and 0<=a.warmup<=30 and 1<=a.repeats<=10):
        p.error('Safe bounds: rate 1..2000, duration 1..120, warmup 0..30, repeats 1..10')
    if any(c not in ((1,) if defect else (32,128,512)) for c in a.concurrency) or any(c not in (1,100,10000) for c in a.clients):
        p.error('Use concurrency 32/128/512 and clients 1/100/10000')
    if a.plan:
        cells=sum(variants.supports(v,backend,mode) for v,(rate,backend),mode in itertools.product(a.variants,workload_axes,a.modes))
        rounds=cells*len(a.clients)*len(a.concurrency)*a.repeats
        quotas=[dict(backend=b,rate=r,clients=c,mode=m,quota=5 if defect else workloads.quota(m,r,a.duration,a.warmup,c))
                for (r,b),c,m in itertools.product(workload_axes,a.clients,a.modes)]
        print(json.dumps(dict(rounds=rounds,rates=rates,rates_by_backend=rates_by_backend,clients=a.clients,concurrency=a.concurrency,modes=a.modes,repeats=a.repeats,measurement_seconds=a.duration,warmup_seconds=a.warmup,load_seconds=rounds*(a.duration+a.warmup),quotas=quotas,note='Build, startup, sampling and cleanup time are additional'),indent=2))
        return
    if a.profile not in ('smoke','defect') and a.skip_build:
        p.error('--skip-build is limited to smoke/defect; formal runs must rebuild from the recorded source')
    if a.reuse_build_manifest and (not compare or a.skip_build):
        p.error('Verified build reuse is exclusive to compare and cannot be combined with --skip-build')
    dirty = run('git','status','--porcelain',cwd=ROOT)
    if dirty and not a.allow_dirty:
        p.error('Dirty tree: commit changes or explicitly use --allow-dirty (results marked non-reproducible)')
    # Lock shared by all invocations from this checkout; no parallel benchmark services.
    lock = open(HERE/'results.lock','w')
    fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
    info=json.loads(docker('info','--format','{{json .}}'))
    for capability in ('MemoryLimit','SwapLimit','CpuCfsQuota','CPUSet'):
        if not info.get(capability):
            p.error('Required Docker capability unavailable: '+capability)
    def cpus(value):
        found=set()
        for token in value.split(','):
            if '-' in token:
                start,end=map(int,token.split('-'));found.update(range(start,end+1))
            else: found.add(int(token))
        return found
    try:
        resource=resources.layout(a.resources,info['NCPU'])
    except ValueError as error:
        p.error(str(error))
    sets=[cpus(os.environ.get(k,resource[v])) for k,v in [('LOADGEN_CPUSET','loadgen_cpuset'),('SERVER_CPUSET','server_cpuset'),('REDIS_CPUSET','redis_cpuset')]]
    if any(not x or min(x)<0 or max(x)>=info['NCPU'] for x in sets):
        p.error('Invalid CPU set')
    if a.resources in ('safe','comparison') and ([len(x) for x in sets]!=([2,1,1] if compare else [1,2,1]) or any(x&y for x,y in itertools.combinations(sets,2))):
        p.error('CPU sets require disjoint 2/1/1 CPUs for high-load comparison or 1/2/1 for Safe')
    for key, selected in zip(('loadgen_cpuset','server_cpuset','redis_cpuset'),sets):
        resource[key]=','.join(map(str,sorted(selected)))
    resource['effective_cpusets']=[sorted(x) for x in sets]
    resource['shared_cpuset']=any(x&y for x,y in itertools.combinations(sets,2))
    if info['MemTotal']<4*1024**3:
        p.error('Requires at least 4 GiB Docker RAM')
    if info['CgroupVersion']!='2':
        p.error('This collector requires cgroup v2; resource limits will not be disabled')
    a.output.mkdir(parents=True,exist_ok=True)
    outsiders = docker('ps','--format','{{.ID}} {{.Names}}')
    if outsiders:
        print('Warning: other running containers may interfere; recorded in environment.json',flush=True)
    metadata=dict(git_sha=run('git','rev-parse','HEAD',cwd=ROOT),dirty=dirty,
                  platform=platform.platform(),docker=info,profile=vars(a)|{'output':str(a.output)},
                  resource_layout=resource, other_running_containers=outsiders, rust='1.98.0', build='release --locked', http_keep_alive_seconds=120,competitors='governor 0.10.0; trypema 0.1.2/core 2.2.1; extensible 0.4.0; Actix 4.18.1')
    metadata['runner']={key:os.environ.get(key) for key in ('RUNNER_OS','RUNNER_ARCH','RUNNER_NAME','ImageOS','ImageVersion','GITHUB_RUN_ID','GITHUB_RUN_ATTEMPT','GITHUB_JOB','BENCHMARK_BACKENDS')}
    metadata['cpu_model']=platform.processor()
    if Path('/proc/cpuinfo').is_file():
        metadata['cpu_model']=next((line.split(':',1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith(('model name','Hardware'))),metadata['cpu_model'])
    elif platform.system()=='Darwin':
        metadata['cpu_model']=run('sysctl','-n','machdep.cpu.brand_string')
    metadata['release_tag_commits']={v:run('git','rev-parse',v+'^{commit}',cwd=ROOT) for v in a.variants if v.startswith('v') and subprocess.run(['git','rev-parse','--verify',v+'^{commit}'],cwd=ROOT,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0}
    metadata['source_hashes']={name:hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in run('git','ls-files','--cached','--others','--exclude-standard',cwd=ROOT).splitlines() if (ROOT/name).is_file()}
    (a.output/'environment.json').write_text(json.dumps(metadata,indent=2))
    if dirty:
        (a.output/'working-tree.patch').write_text(run('git','diff','HEAD',cwd=ROOT))
    for v in a.variants:
        lockfile=HERE/'adapters'/v/'Cargo.lock'
        if not lockfile.exists():
            p.error('Missing checked-in lockfile: '+str(lockfile))
        (a.output/(v+'.Cargo.lock')).write_bytes(lockfile.read_bytes())
        if not a.skip_build and not a.reuse_build_manifest:
            subprocess.run(['docker','build','-f',str(HERE/'docker/server.Dockerfile'),'--build-arg','VARIANT='+v,'-t','ratelimit-bench-'+v+':local',str(ROOT)],check=True)
    if compare:
        (a.output/'loadgen.Cargo.lock').write_bytes((HERE/'native-loadgen/Cargo.lock').read_bytes())
    generator_image='ratelimit-bench-loadgen-native:local' if compare else 'ratelimit-bench-loadgen:local'
    generator_dockerfile='native-loadgen.Dockerfile' if compare else 'loadgen.Dockerfile'
    if not a.skip_build and not a.reuse_build_manifest:
        subprocess.run(['docker','build','-f',str(HERE/'docker'/generator_dockerfile),'-t',generator_image,str(ROOT)],check=True)
    images={v:json.loads(docker('image','inspect','ratelimit-bench-'+v+':local'))[0] for v in a.variants}
    images['loadgen']=json.loads(docker('image','inspect',generator_image))[0]
    if a.reuse_build_manifest:
        source_path=Path(a.reuse_build_manifest)
        verify_reuse(metadata,json.loads(source_path.read_text()),images)
        metadata['build_reuse']=dict(manifest=str(source_path),sha256=hashlib.sha256(source_path.read_bytes()).hexdigest())
    native={'aarch64':'arm64','x86_64':'amd64'}.get(info['Architecture'],info['Architecture'])
    if any(i['Architecture']!=native for i in images.values()):
        raise RuntimeError('Non-native image architecture; emulation is not permitted')
    metadata['images']=images
    (a.output/'environment.json').write_text(json.dumps(metadata,indent=2))
    project='rlbench-'+uuid.uuid4().hex[:12]
    metadata['compose_project']=project
    (a.output/'environment.json').write_text(json.dumps(metadata,indent=2))
    env=os.environ.copy()
    env['LOADGEN_IMAGE']=generator_image
    env['PHASE_BARRIER']='1' if compare else '0'
    for key in ('server_cpus','loadgen_cpus','redis_cpus','workers','server_cpuset','loadgen_cpuset','redis_cpuset'):
        env.setdefault(key.upper(),str(resource[key]))
    # CPU quotas are authoritative for the selected layout.
    for key in ('server_cpus','loadgen_cpus','redis_cpus','workers'):
        env[key.upper()]=str(resource[key])
    compose=['docker','compose','-p',project,'-f',str(HERE/'compose.yaml')]
    def dc(*args): return run(*compose,*args,env=env)
    project_live=False
    def cleanup():
        nonlocal project_live
        if not project_live: return
        result=subprocess.run(compose+['down','--volumes','--remove-orphans'],env=env,check=False,stdout=subprocess.DEVNULL)
        remaining=docker('ps','-aq','--filter','label=com.docker.compose.project='+project)
        if result.returncode or remaining:
            raise RuntimeError('Benchmark cleanup failed for '+project+'; inspect only this project before continuing')
        project_live=False
    def interrupted(*_): raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM,interrupted)
    rounds=[]
    platform_id=hashlib.sha256(json.dumps([platform.node(),info['ID'],info['Architecture'],info['NCPU'],info['MemTotal']]).encode()).hexdigest()[:16]
    try:
        for (rate,backend),clients,concurrency,mode in itertools.product(workload_axes,a.clients,a.concurrency,a.modes):
            a.rate=rate
            # Forward/reverse blocks plus independent baseline repeats provide A/A calibration.
            for repeat in range(a.repeats):
                order=list(a.variants) if repeat%2==0 else list(reversed(a.variants))
                for v in order:
                    if not variants.supports(v,backend,mode):
                        continue
                    duration=a.duration
                    limit=workloads.quota(mode,a.rate,duration,a.warmup,clients)
                    if defect: limit=5
                    env.update(SERVER_IMAGE='ratelimit-bench-'+v+':local',BACKEND=backend,LIMIT=str(limit),WINDOW=str(duration+a.warmup+60),RATE=str(a.rate),DURATION=str(duration),WARMUP=str(a.warmup),CLIENTS=str(clients),CONCURRENCY=str(concurrency))
                    print(f'rate={rate} {v} {backend} clients={clients} concurrency={concurrency} mode={mode} repeat={repeat}',flush=True)
                    try:
                        project_live=True
                        dc('up','-d','--wait','server')
                        ids={s:dc('ps','-q',s) for s in ('server','redis')}
                        inspections={s:json.loads(docker('inspect',cid))[0] for s,cid in ids.items()}
                        for s,cpu,mem in [('server',resource['server_cpus'],1024**3),('redis',resource['redis_cpus'],512*1024**2)]:
                            h=inspections[s]['HostConfig']
                            if h['NanoCpus']!=cpu*10**9 or h['Memory']!=mem or h['MemorySwap']!=mem:
                                raise RuntimeError('Resource limits not applied: '+s)
                        before={s:cgroup(cid) for s,cid in ids.items()} if not compare else None
                        diagnostic_start=time.monotonic()
                        # Stream scoped stats concurrently; never delay measurement completion to poll.
                        with tempfile.TemporaryFile(mode='w+') as stats_file:
                            sampler=subprocess.Popen(['docker','stats','--format','{{json .}}',*ids.values()],stdout=stats_file,stderr=stats_file,text=True)
                            process=None
                            try:
                                process=subprocess.Popen(compose+['run','--no-deps','--name',project+'-loadgen','-T','loadgen'],env=env,stdout=subprocess.PIPE,stdin=subprocess.PIPE if compare else subprocess.DEVNULL,text=True)
                                if compare:
                                    result,before,after,diagnostic_seconds=controlled_measurement(process,lambda:{s:cgroup(cid) for s,cid in ids.items()})
                                else:
                                    output=process.communicate()[0]
                                if process.returncode: raise RuntimeError('Load generator failed')
                                if sampler.poll() not in (None,0):
                                    raise RuntimeError('Docker stats sampler failed')
                            finally:
                                for child in (process,sampler):
                                    if child is not None and child.poll() is None:
                                        child.terminate()
                                        try: child.wait(timeout=5)
                                        except subprocess.TimeoutExpired: child.kill();child.wait()
                                if process is not None:
                                    if process.stdin: process.stdin.close()
                                    if process.stdout: process.stdout.close()
                            stats_file.seek(0)
                            samples=stats_samples(stats_file.read())
                        if not compare: result=json.loads(output.splitlines()[-1])
                        ids['loadgen']=project+'-loadgen'
                        if not compare: after={s:cgroup(cid) for s,cid in ids.items() if s!='loadgen'}
                        # Stopped containers retain OOM state; cgroup disappears after exit.
                        states={s:json.loads(docker('inspect',cid))[0] for s,cid in ids.items()}
                        if states['loadgen']['HostConfig']['Memory']!=512*1024**2 or states['loadgen']['HostConfig']['NanoCpus']!=resource['loadgen_cpus']*10**9:
                            raise RuntimeError('Load generator limits not applied')
                        redis_info=dc('exec','-T','redis','redis-cli','INFO','ALL')
                        rc=dict(line.split(':',1) for line in redis_info.splitlines() if ':' in line and not line.startswith('#'))
                        cpu={s:{k:counters(after[s]).get(k,0)-counters(before[s]).get(k,0) for k in ('usage_usec','throttled_usec','nr_periods','nr_throttled')} for s in before}
                        if not compare: diagnostic_seconds=time.monotonic()-diagnostic_start
                        throttled=any(x['throttled_usec']>duration*100000 for x in cpu.values())
                        lg_before=counters(result.get('loadgen_cgroup_before',{}).get('cpu.stat',''))
                        lg_after=counters(result['loadgen_cgroup']['cpu.stat'])
                        lg_cpu={key:lg_after.get(key,0)-lg_before.get(key,0) for key in ('usage_usec','throttled_usec','nr_periods','nr_throttled')}
                        result.update(loadgen_cpu_limit=resource['loadgen_cpus'],loadgen_cpu_pct=lg_cpu['usage_usec']/max(result['elapsed_seconds'],.001)/10000,
                                      loadgen_throttled_usec=lg_cpu['throttled_usec'],
                                      warmup_seconds=a.warmup, resource_interval_seconds=diagnostic_seconds,
                                      resource_phase='measurement' if compare else 'warmup-and-measurement',
                                      resource_capture_skew_seconds=max(0,diagnostic_seconds-result['elapsed_seconds']) if compare else None,
                                      resource_profile=a.resources, server_cpu_limit=resource['server_cpus'],
                                      platform_id=platform_id,variant=v,scenario=f'{backend}/{clients}/{concurrency}/{mode}/{rate}rps',repeat=repeat,
                                      workload_clients=clients,workload_mode=mode,rejection_fraction=workloads.rejection_fraction(result),
                                      oom=any(x['State']['OOMKilled'] for x in states.values()) or any(counters(raw).get('oom',0)>0 for raw in after.values()) or counters(result['loadgen_cgroup']['memory.events']).get('oom',0)>0,redis_oom=int(rc.get('errorstat_OOM','count=0').split('=')[-1])>0,
                                      cpu_throttled=throttled and not compare, quota_throttled=throttled,
                                      loadgen_peak_bytes=int(result['loadgen_cgroup']['memory.peak']),
                                      server_cpu_pct=cpu['server']['usage_usec']/(result['elapsed_seconds'] if compare else diagnostic_seconds)/10000,
                                      server_rss_bytes=int(after['server'].split('VmRSS:')[1].split()[0])*1024,
                                      redis_memory_bytes=int(rc['used_memory']), redis_connections=int(rc['connected_clients']),
                                      correctness_mismatch=variants.VARIANTS[v]['oracle'] and result['http_200']!=result['expected_admissions'],
                                      memory_peak_bytes=int(after['server'].split('MEMORY\n')[1].splitlines()[1]),
                                      quota_exceeded=variants.VARIANTS[v]['oracle'] and result['max_admissions_per_client']>limit,
                                      observed_nominal_excess=max(0,result['max_admissions_per_client']-limit),
                                      http_version='4.18.1',http_keep_alive_seconds=120,resource_layout_id=hashlib.sha256(json.dumps(resource,sort_keys=True).encode()).hexdigest()[:12],
                                      quota=limit,window_seconds=duration+a.warmup+60,
                                      algorithm=variants.VARIANTS[v]['algorithm'], workload='comparison' if compare else 'cost',
                                      equivalence_group='exact-log' if v in ('head','v0.2.1') else v,
                                      correctness='first-window burst oracle' if variants.VARIANTS[v]['oracle'] else 'non-equivalent policy; observations only',
                                      diagnostics=dict(before=before,after=after,cpu=cpu,samples=samples,redis=rc,containers=states))
                        print(round_diagnostic(result,a.rate),flush=True)
                        if generator_pressure(result):
                            print('  !! load generator pressure: late drops or CPU near quota; this round cannot qualify a rate',flush=True)
                        rounds.append(result)
                    except Exception as error:
                        rounds.append(dict(variant=v,scenario=f'{backend}/{clients}/{concurrency}/{mode}/{rate}rps',failure=str(error)))
                        (a.output/'raw.json').write_text(json.dumps({'rounds':rounds},indent=2))
                        (a.output/'report.md').write_text(report.markdown(report.summarize({'rounds':rounds})))
                        (a.output/'assessment.md').write_text(assessment.render(a.output,'failure'))
                        raise
                    finally:
                        cleanup()
                    (a.output/'raw.json').write_text(json.dumps({'rounds':rounds},indent=2))
                    summary=report.summarize({'rounds':rounds})
                    (a.output/'validated.json').write_text(json.dumps(summary,indent=2))
                    (a.output/'report.md').write_text(f"Git SHA: {metadata['git_sha']}\n\nDirty/non-reproducible: {bool(dirty)}\n\n"+report.markdown(summary))
                    if report.invalid_comparison_baseline(result):
                        (a.output/'assessment.md').write_text(assessment.render(a.output,'measurement_failure'))
                        raise RuntimeError('Baseline measurement invalid; stopped high-load comparison before wasting further rounds. Inspect transport/scheduler diagnostics.')
    finally:
        cleanup()
    (a.output/'assessment.md').write_text(assessment.render(a.output,'completed'))
    print(a.output/'report.md')
    if defect and not any(assessment.expected_historical(r) for r in rounds):
        raise SystemExit('Historical defect not reproduced; do not claim success')
    if a.profile in ('smoke','defect') and any(report.validate(r) and not assessment.expected_historical(r) for r in rounds):
        raise SystemExit('Smoke validation failed; inspect report.md and raw.json')

if __name__=='__main__':
    main()
