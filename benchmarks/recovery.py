"""Bounded recovery of load-generator scheduling failures.

Only generator-related measurements may be retried at lower offered rates.
Correctness errors, corrupted measurements and process failures fail closed.
"""

SCHEDULER_EXIT = 75
CORE = ('baseline', 'head')


class SchedulerAbort(RuntimeError):
    """The underlying compare process explicitly stopped on baseline scheduler loss."""

    def __init__(self, rows):
        self.rows = rows
        super().__init__('baseline missed scheduled arrivals')


def inspect(rows, expected_repeats, validate, exit_code=0):
    """Return recoverable generator reason, or None; raise for code/data faults.

    Invalid comparison data is *never* converted to a successful measurement.
    A recoverable result must be stored separately and not used for verdicts.
    """
    if exit_code not in (0, SCHEDULER_EXIT):
        raise RuntimeError(f'Benchmark process failed (exit {exit_code})')
    core = [r for r in rows if r.get('variant') in CORE]
    if exit_code == SCHEDULER_EXIT:
        if not any(r.get('variant') == 'baseline' and
                   set(validate(r)) == {'scheduler_drops'} for r in core):
            raise RuntimeError('Scheduler exit without a qualifying baseline record')
    recoverable = []
    for row in core:
        reasons = set(validate(row))
        if reasons == {'scheduler_drops'}:
            recoverable.append(f"{row['variant']} scheduler_drops={row.get('dropped_late', 0)}")
        elif reasons:
            raise RuntimeError(f"{row['variant']} invalid measurement: {', '.join(sorted(reasons))}")
        cpu_limit = row.get('loadgen_cpu_limit', 0)
        cpu_pct = row.get('loadgen_cpu_pct', 0)
        if cpu_limit > 0 and cpu_pct >= 85 * cpu_limit:
            recoverable.append(f"{row['variant']} loadgen CPU {cpu_pct:.1f}% of {cpu_limit} cores")
    if exit_code == SCHEDULER_EXIT:
        return '; '.join(recoverable)
    for variant in CORE:
        cells = [row for row in core if row['variant'] == variant]
        if (len(cells) != expected_repeats or
            {r.get('repeat') for r in cells} != set(range(expected_repeats))):
            raise RuntimeError(f'{variant} has incomplete/duplicate repeats; no confirmation')
    return '; '.join(recoverable) if recoverable else None


def lower_rate(rate):
    """Reduce rate by about 20%, retaining 500-RPS step and a 5k floor."""
    lower = max(5000, int((rate * 0.8) // 500) * 500)
    return lower if lower < rate else None


def confirm_at_safe_rate(initial_rate, preflight, confirm, validate,
                         confirmation_repeats=5, max_downshifts=2):
    """Never reuse failed rounds or cherry-pick repeats to qualify a rate.

    preflight(rate, attempt) must use one long HEAD/baseline repetition, or be
    None for a short operational check. confirm(rate, attempt) must use the full
    configured fixed repetition count; both callbacks preserve their raw data.
    """
    events = []
    rate = initial_rate
    for attempt in range(max_downshifts + 1):
        issue = None
        for phase, runner, repeats in (('preflight', preflight, 1),
                                      ('confirm', confirm, None)):
            if runner is None:
                continue
            try:
                rows = runner(rate, attempt)
                issue = inspect(rows, repeats if repeats is not None else confirmation_repeats,
                                validate)
            except SchedulerAbort as abort:
                rows = abort.rows
                issue = inspect(rows, 1, validate, exit_code=SCHEDULER_EXIT)
            if issue:
                break
        if not issue:
            return rate, rows, events
        next_rate = lower_rate(rate) if attempt < max_downshifts else None
        events.append(dict(phase=phase, rate=rate, issue=issue,
                           next_rate=next_rate, excluded_from_verdicts=True))
        if next_rate is None:
            break
        rate = next_rate
    return None, None, events
