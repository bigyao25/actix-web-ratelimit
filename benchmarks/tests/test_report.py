import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("report", Path(__file__).parents[1] / "report.py")
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.round = dict(platform_id="host-a", variant="head", scenario="memory",
                          offered_rps=100, achieved_rps=90, p50_ms=1, p95_ms=2, p99_ms=3,
                          http_200=90, http_429=0, http_503=0, http_other=0, errors=0,
                          oom=False, loadgen_saturated=False, redis_oom=False,
                          cpu_throttled=False, memory_peak_bytes=100, quota_exceeded=False)

    def test_valid_rounds_are_grouped_by_platform(self):
        result = report.summarize({"rounds": [self.round, {**self.round, "platform_id": "host-b"}]})
        self.assertEqual(len(result["summary"]), 2)

    def test_missing_and_overadmission_are_excluded(self):
        result = report.summarize({"rounds": [{}, {**self.round, "quota_exceeded": True}]})
        self.assertFalse(result["summary"])
        self.assertIn("quota_exceeded", result["rounds"][1]["invalid_reasons"])

    def test_error_and_invalid_percentiles_are_excluded(self):
        self.assertTrue(report.validate({**self.round, "http_503": 1}))
        self.assertTrue(report.validate({**self.round, "p99_ms": 0}))

class OutputTest(unittest.TestCase):
    def test_all_invalid_report_still_renders(self):
        result = report.summarize({'rounds': [{'variant': 'legacy', 'quota_exceeded': True}]})
        self.assertIn('Excluded rounds: 1', report.markdown(result))

    def test_correctness_mismatch_is_not_ranked(self):
        fixture = ReportTest(); fixture.setUp()
        result = report.summarize({'rounds': [{**fixture.round, 'correctness_mismatch': True}]})
        self.assertFalse(result['summary'])


class QualityTest(unittest.TestCase):
    def setUp(self):
        fixture = ReportTest(); fixture.setUp()
        self.round=fixture.round

    def test_single_round_has_no_repeat_spread(self):
        result=report.summarize({'rounds':[self.round]})
        self.assertIsNone(result['summary'][0]['aa_spread_pct'])
        self.assertTrue(result['summary'][0]['warnings'])

    def test_different_rates_are_never_combined(self):
        result=report.summarize({'rounds':[{**self.round,'target_rps':100},{**self.round,'target_rps':500}]})
        self.assertEqual(len(result['summary']),2)

    def test_inconsistent_accounting_is_invalid(self):
        r={**self.round,'schema_version':2,'sample_count':90,'scheduled':100,'dropped':0,
           'mean_in_flight':1,'peak_in_flight':1,'concurrency_limit':32}
        self.assertIn('request accounting mismatch',report.validate(r))

class SchemaThreeTest(unittest.TestCase):
    def fixture(self):
        fixture=ReportTest();fixture.setUp()
        return {**fixture.round,'schema_version':3,'scheduled':100,'sample_count':90,
                'dropped':10,'dropped_capacity':10,'dropped_late':0,'loadgen_saturated':True,
                'error_categories':{},'mean_in_flight':1,'peak_in_flight':2,'concurrency_limit':32}

    def test_capacity_drop_is_clean_overload_not_invalid_measurement(self):
        row=self.fixture()
        self.assertFalse(report.validate(row))
        self.assertIn('in-flight limit reached',report.load_failures(row))
        result=report.summarize({'rounds':[row]})
        self.assertFalse(result['summary'])
        self.assertFalse(result['rounds'][0]['invalid_reasons'])

    def test_scheduler_drop_is_still_invalid(self):
        row={**self.fixture(),'dropped_capacity':0,'dropped_late':10}
        self.assertIn('scheduler_drops',report.validate(row))

    def test_errors_must_match_their_categories(self):
        row={**self.fixture(),'errors':1,'http_200':89}
        self.assertIn('error category accounting mismatch',report.validate(row))
        row['error_categories']={'read_status:eof':1}
        self.assertNotIn('error category accounting mismatch',report.validate(row))
        self.assertIn('errors',report.validate(row))

    def test_accounting_is_checked_even_when_transport_failed(self):
        row={**self.fixture(),'errors':1,'error_categories':{'read_status:eof':1}}
        self.assertIn('request accounting mismatch',report.validate(row))

    def test_baseline_aborts_on_bad_measurement_but_not_clean_overload(self):
        row={**self.fixture(),'variant':'baseline','workload':'comparison'}
        self.assertFalse(report.invalid_comparison_baseline(row))
        row.update(errors=1,http_200=89,error_categories={'read_status:eof':1})
        self.assertTrue(report.invalid_comparison_baseline(row))
        row['variant']='head'
        self.assertFalse(report.invalid_comparison_baseline(row))


if __name__ == "__main__":
    unittest.main()
