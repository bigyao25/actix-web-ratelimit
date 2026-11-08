"""Scheduling-loss recovery is bounded and never turns invalid rounds into wins."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0,str(Path(__file__).parents[1]))
import recovery


def row(variant, repeat=0, reasons=(), **changes):
    item=dict(variant=variant,repeat=repeat,reasons=list(reasons),
              loadgen_cpu_limit=2,loadgen_cpu_pct=110)
    item.update(changes)
    return item


class RecoveryTest(unittest.TestCase):
    def inspect(self, rows, n=1, code=0):
        return recovery.inspect(rows,n,lambda r:r.get('reasons',[]),code)

    def test_clean(self):
        self.assertIsNone(self.inspect([row('baseline'),row('head')]))

    def test_explicit_baseline_abort(self):
        message=self.inspect([row('baseline',reasons=['scheduler_drops'],
                                  dropped_late=310)],code=75)
        self.assertIn('310',message)

    def test_head_scheduler_drop(self):
        self.assertIn('97',self.inspect([row('baseline'),row('head',
                   reasons=['scheduler_drops'],dropped_late=97)]))

    def test_load_generator_cpu_pressure(self):
        self.assertIn('CPU',self.inspect([row('baseline'),
                       row('head',loadgen_cpu_pct=180)]))

    def test_corrupt_core_data_fails_closed(self):
        with self.assertRaises(RuntimeError):
            self.inspect([row('baseline'),row('head',
                          reasons=['request accounting mismatch'])])

    def test_correctness_failure_fails_closed(self):
        with self.assertRaises(RuntimeError):
            self.inspect([row('baseline'),row('head',
                          reasons=['correctness_mismatch'])])

    def test_incomplete_repeats_block(self):
        with self.assertRaises(RuntimeError):
            self.inspect([row('baseline'),row('head')],n=5)

    def test_false_scheduler_exit_blocked(self):
        with self.assertRaises(RuntimeError):
            self.inspect([row('baseline')],code=75)

    def test_unexpected_exit_blocked(self):
        with self.assertRaises(RuntimeError):
            self.inspect([row('baseline'),row('head')],code=1)

    def test_rate_reduction_is_bounded(self):
        self.assertEqual(recovery.lower_rate(49000),39000)
        self.assertEqual(recovery.lower_rate(39000),31000)
        self.assertIsNone(recovery.lower_rate(5000))


class RecoveryCampaignTest(unittest.TestCase):
    def validate(self,row): return row.get('reasons',[])

    def success(self,rate,attempt):
        return [row(v,repeat=i) for i in range(5)
                for v in ('baseline','head')]

    def test_preflight_scheduler_aborts_then_downshifts(self):
        called=[]
        def preflight(rate,attempt):
            called.append(('preflight',rate))
            if rate==49000:
                raise recovery.SchedulerAbort(
                    [row('baseline',reasons=['scheduler_drops'],dropped_late=310)])
            return [row('baseline'),row('head')]
        def confirm(rate,attempt):
            called.append(('confirm',rate))
            return self.success(rate,attempt)
        rate,rows,events=recovery.confirm_at_safe_rate(
            49000,preflight,confirm,self.validate)
        self.assertEqual(rate,39000)
        self.assertEqual(len(rows),10)
        self.assertEqual([e['rate'] for e in events],[49000])
        self.assertEqual(called,[('preflight',49000),
                                 ('preflight',39000),('confirm',39000)])

    def test_full_confirmation_scheduler_loss_downshifts(self):
        def preflight(rate,attempt):return [row('baseline'),row('head')]
        def confirm(rate,attempt):
            if rate==49000:
                raise recovery.SchedulerAbort(
                    [row('baseline',reasons=['scheduler_drops'],dropped_late=310)])
            return self.success(rate,attempt)
        rate,rows,events=recovery.confirm_at_safe_rate(
            49000,preflight,confirm,self.validate)
        self.assertEqual(rate,39000)
        self.assertEqual(events[0]['phase'],'confirm')

    def test_exhausted_recovery_is_inconclusive(self):
        def preflight(rate,attempt):
            return [row('baseline'),row('head',
                        reasons=['scheduler_drops'],dropped_late=2)]
        rate,rows,events=recovery.confirm_at_safe_rate(
            49000,preflight,self.success,self.validate)
        self.assertIsNone(rate)
        self.assertIsNone(rows)
        self.assertEqual([e['rate'] for e in events],[49000,39000,31000])
        self.assertIsNone(events[-1]['next_rate'])

    def test_non_scheduler_error_is_not_recoverable(self):
        def preflight(rate,attempt):
            return [row('baseline'),row('head',
                        reasons=['correctness_mismatch'])]
        with self.assertRaises(RuntimeError):
            recovery.confirm_at_safe_rate(
                49000,preflight,self.success,self.validate)

    def test_smoke_skips_long_preflight_and_downshifts(self):
        def short(rate,attempt):return [row('baseline'),row('head')]
        rate,rows,events=recovery.confirm_at_safe_rate(
            15000,None,short,self.validate,confirmation_repeats=1,
            max_downshifts=0)
        self.assertEqual(rate,15000)
        self.assertFalse(events)


if __name__=='__main__':
    unittest.main()
