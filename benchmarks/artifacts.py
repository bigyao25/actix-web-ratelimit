#!/usr/bin/env python3
"""Inventory benchmark files; optionally remove only local regenerable caches."""
import argparse
import json
from pathlib import Path
import shutil

HERE=Path(__file__).resolve().parent


def cache_paths():
    paths=list((HERE/'adapters').glob('*/target'))+list(HERE.rglob('__pycache__'))
    # A target can contain Python caches: only remove outermost selected roots.
    return [p for p in paths if not any(other!=p and other in p.parents for other in paths)]


def size(path):
    return sum(p.stat().st_size for p in path.rglob('*') if p.is_file() and not p.is_symlink())


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--clean-local-cache', action='store_true', help='Delete adapter target/ and Python caches; keep source, locks and reports')
    args=parser.parse_args()
    paths=cache_paths()
    for path in paths:
        if path.is_symlink() or path.resolve()!=path:
            raise SystemExit('Refusing a symlinked cache path: '+str(path))
    inventory=dict(caches=[dict(path=str(p),bytes=size(p)) for p in paths],
                   reports_bytes=size(HERE/'results') if (HERE/'results').exists() else 0,
                   note='Docker images/build caches and shared Cargo cache are not deleted by this tool')
    print(json.dumps(inventory,indent=2))
    if args.clean_local_cache:
        for path in paths:
            shutil.rmtree(path)
        print('Removed only local benchmark caches; reports and locks retained.')


if __name__=='__main__':
    main()
