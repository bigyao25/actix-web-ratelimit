"""Assessment must distinguish missing data, expected defects and capacity evidence."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0,str(Path(__file__).parents[1]))
import summary
import resources

class SummaryTest(unittest.TestCase):
    def fixture(self):
        return dict(platform_id='test',variant='head',scenario='memory/1/32/allow/500rps',
                    offered_rps=500,achieved_rps=499,p50_ms=1,p95_ms=2,p99_ms=3,
                    http_200=1000,http_429=0,http_503=0,http_other=0,errors=0,
                    oom=False,loadgen_saturated=False,redis_oom=False,cpu_throttled=False,
                    memory_peak_bytes=100,quota_exceeded=False,target_rps=500)

    def test_missing_results_never_claim_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            content=summary.render(Path(tmp),'failure')
        self.assertIn('No benchmark data collected',content)
        self.assertNotIn('RPS validated',content)

    def test_historical_oom_is_not_an_expected_defect(self):
        self.assertTrue(summary.expected_historical({**self.fixture(),'variant':'v0.1.2','quota_exceeded':True}))
        self.assertFalse(summary.expected_historical({**self.fixture(),'variant':'v0.1.2','quota_exceeded':True,'oom':True}))

    def test_loadgen_pressure_does_not_become_server_capacity(self):
        self.assertIn('loadgen pressure',summary.assess({**self.fixture(),'loadgen_cpu_limit':.5,'loadgen_cpu_pct':49,'loadgen_saturated':True}))
        self.assertEqual(summary.assess(self.fixture()),'no bottleneck demonstrated')

    def test_success_step_resolves_inconclusive_environment_status(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'result-classification.json').write_text(json.dumps({
                'status':'inconclusive_environment'}))
            self.assertEqual(summary.resolve_status(folder,'success'),'inconclusive_environment')
            self.assertEqual(summary.resolve_status(folder,'failure'),'failure')

    def test_smoke_summary_contains_qualifiers_and_artifact(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[self.fixture()]}))
            (folder/'environment.json').write_text(json.dumps({'profile':{'profile':'smoke','resources':'ci'},'resource_layout':{'shared_cpuset':True}}))
            content=summary.render(folder,'success','https://github.com/example/artifact')
        self.assertIn('not a capacity estimate',content)
        self.assertIn('CI smoke',content)
        self.assertIn('shares CPU sets explicitly',content)
        self.assertIn('https://github.com/example/artifact',content)

class ResourceTest(unittest.TestCase):
    def test_two_cpu_ci_is_explicit_and_quota_bounded(self):
        config=resources.layout('ci',2)
        self.assertTrue(config['shared_cpuset'])
        self.assertEqual(sum(config[k] for k in ('server_cpus','loadgen_cpus','redis_cpus')),2)
        with self.assertRaises(ValueError):resources.layout('safe',2)

    def test_four_cpu_ci_can_avoid_sharing(self):
        self.assertFalse(resources.layout('ci',4)['shared_cpuset'])


class ComparisonTest(unittest.TestCase):
    def test_comparison_layout_reserves_generator_capacity(self):
        config=resources.layout('comparison',4)
        self.assertEqual((config['loadgen_cpus'],config['server_cpus'],config['redis_cpus']),(2,1,1))
        self.assertFalse(config['shared_cpuset'])
        with self.assertRaises(ValueError): resources.layout('comparison',3)

    def test_comparison_never_qualifies_single_round(self):
        fixture=SummaryTest().fixture()
        fixture.update(workload='comparison',loadgen_cpu_pct=10,loadgen_cpu_limit=2,scheduled_latency={'p99_ms':1})
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[fixture]}))
            (folder/'environment.json').write_text(json.dumps({'profile':{'profile':'compare'}}))
            content=summary.render(folder,'success')
        self.assertIn('High-load benchmark qualification',content)
        self.assertIn('fewer than five repeats',content)
        self.assertNotIn('all criteria met',content)

class QualitySummaryTest(unittest.TestCase):
    def test_baseline_errors_prioritize_client_diagnostics(self):
        fixture=SummaryTest().fixture()
        fixture.update(variant='baseline',errors=1,error_categories={'read_status:eof':1})
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[fixture]}))
            content=summary.render(folder,'success')
        self.assertIn('Measurement quality: **failed**',content)
        self.assertIn('Baseline also has transport errors',content)
        self.assertIn('read_status:eof',content)
        self.assertNotIn('If no pressure was demonstrated',content)

    def test_clean_capacity_limited_round_is_not_a_clean_pass(self):
        from test_report import SchemaThreeTest
        fixture=SchemaThreeTest().fixture()
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[fixture]}))
            content=summary.render(folder,'success')
        self.assertIn('measurement-clean: **1**',content)
        self.assertIn('Clean rounds failing load criteria: **1**',content)
        self.assertIn('In-flight bounds were reached',content)


class DefectSummaryTest(unittest.TestCase):
    def test_expected_defect_has_no_performance_next_step(self):
        row=SummaryTest().fixture()
        row.update(variant='v0.1.2',quota_exceeded=True)
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[row]}))
            (folder/'environment.json').write_text(json.dumps({'profile':{'profile':'defect'}}))
            content=summary.render(folder,'completed')
        self.assertIn('historical defect reproduced',content)
        self.assertIn('one-shot historical defect proof',content)
        self.assertNotIn('use a bounded local',content)
