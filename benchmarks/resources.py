"""Explicit Docker resource layouts; CI sharing never silently removes limits."""


def layout(profile, cpus):
    if profile=='pk':
        if cpus<4:
            raise ValueError('PK requires four native Docker CPUs; limits will not be shared or removed')
        return dict(server_cpus=1,loadgen_cpus=2,redis_cpus=1,workers=1,
                    server_cpuset='2',loadgen_cpuset='0,1',redis_cpuset='3',shared_cpuset=False)
    if profile=='ci':
        if cpus<2:
            raise ValueError('CI requires at least two Docker CPUs')
        return dict(server_cpus=1,loadgen_cpus=.5,redis_cpus=.5,workers=1,
                    server_cpuset='1',loadgen_cpuset='0',redis_cpuset='2' if cpus>=3 else '0',
                    shared_cpuset=cpus<3)
    if cpus<4:
        raise ValueError('Safe resources require at least four Docker CPUs; CI smoke can use --resources ci')
    return dict(server_cpus=2,loadgen_cpus=1,redis_cpus=1,workers=2,
                server_cpuset='1,2',loadgen_cpuset='0',redis_cpuset='3',shared_cpuset=False)
