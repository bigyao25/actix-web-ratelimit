"""Assessment must distinguish missing data, expected defects and capacity evidence."""
import importlib.util
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


class PKTest(unittest.TestCase):
    def test_pk_layout_reserves_generator_capacity(self):
        config=resources.layout('pk',4)
        self.assertEqual((config['loadgen_cpus'],config['server_cpus'],config['redis_cpus']),(2,1,1))
        self.assertFalse(config['shared_cpuset'])
        with self.assertRaises(ValueError): resources.layout('pk',3)

    def test_pk_never_qualifies_single_round(self):
        fixture=SummaryTest().fixture()
        fixture.update(workload='pk',loadgen_cpu_pct=10,loadgen_cpu_limit=2,scheduled_latency={'p99_ms':1})
        with tempfile.TemporaryDirectory() as tmp:
            folder=Path(tmp)
            (folder/'raw.json').write_text(json.dumps({'rounds':[fixture]}))
            (folder/'environment.json').write_text(json.dumps({'profile':{'profile':'pk'}}))
            content=summary.render(folder,'success')
        self.assertIn('High-load PK qualification',content)
        self.assertIn('fewer than five repeats',content)
        self.assertNotIn('all criteria met',content)
