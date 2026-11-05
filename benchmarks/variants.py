"""Verified adapter capabilities; algorithm equivalence is explicit."""
VARIANTS = {
    'baseline': dict(algorithm='none', backends=['memory','redis'], zero=True, oracle=False),
    'v0.1.2': dict(algorithm='historical sliding log; known defects', backends=['memory','redis'], zero=True, oracle=True),
    'v0.2.1': dict(algorithm='exact sliding log', backends=['memory','redis'], zero=True, oracle=True),
    'head': dict(algorithm='exact sliding log', backends=['memory','redis'], zero=True, oracle=True),
    'governor': dict(algorithm='GCRA', backends=['memory'], zero=False, oracle=False),
    'trypema': dict(algorithm='bucketed sliding counter; best-effort concurrency', backends=['memory','redis'], zero=False, oracle=False),
    'extensible': dict(algorithm='fixed window', backends=['memory','redis'], zero=True, oracle=True),
}

def supports(variant, backend, mode):
    config=VARIANTS[variant]
    return backend in config['backends'] and (mode!='deny' or config['zero'])
