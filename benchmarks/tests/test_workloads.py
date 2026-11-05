"""Synthetic schedules test bounded search, workload coverage and provenance guards."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0,str(Path(__file__).parents[1]))
import bench
import campaign
import capacity
import variants
import verdicts
import workloads
import test_campaign


def probe(variant,rate,limit=52500,**changes):
    return test_campaign.row(variant,rate,http_rps=rate if variant=='baseline' or rate<=limit else rate*.9,**changes)


class MatrixCoverageTest(unittest.TestCase):
    def test_default_isolates_cardinality_and_modes_without_full_product(self):
        cases=workloads.cases()
        self.assertEqual({x['clients'] for x in cases if x['mode']=='allow'},{1,100,10000})
        self.assertEqual({x['mode'] for x in cases if x['clients']==100},set(workloads.MODES))
        self.assertEqual(len(cases),5)
        self.assertEqual(len(workloads.cases(True)),9)

    def test_positive_reject_quota_includes_nonzero_only_competitors(self):
        self.assertEqual(workloads.quota('reject',10000,30,5,100),30)
        self.assertEqual(workloads.quota('mixed',10000,30,5,100),1500)
        self.assertGreater(workloads.quota('allow',10000,30,5,100),300000)
        for peer in ('governor','trypema'):
            self.assertTrue(variants.supports(peer,'memory','reject'))
            self.assertFalse(variants.supports(peer,'memory','deny'))

    def test_realized_client_and_response_coverage_are_required(self):
        r=test_campaign.row('head',workload_clients=10000,clients_observed=100)
        self.assertIn('client cardinality not exercised',workloads.coverage_failures(r))
        r.update(workload_clients=100,clients_observed=100,workload_mode='reject',http_200=10,http_429=990)
        self.assertFalse(workloads.coverage_failures(r))
        r['workload_mode']='mixed'
        self.assertTrue(workloads.coverage_failures(r))
        r.update(http_200=500,http_429=500)
        self.assertFalse(workloads.coverage_failures(r))
        r.update(variant='baseline',http_200=1000,http_429=0)
        self.assertFalse(workloads.coverage_failures(r))

    def test_different_response_mixes_cannot_be_directional_cost_results(self):
        rows=[]
        for i in range(5):
            for peer in ('baseline','head','extensible'):
                r=test_campaign.row(peer,repeat=i,scenario='memory/100/512/mixed/10000rps',
                    http_200=1000 if peer=='baseline' else 300 if peer=='extensible' else 500,
                    http_429=0 if peer=='baseline' else 700 if peer=='extensible' else 500,
                    server_cpu_pct=80 if peer=='extensible' else 40,
                    scheduled_latency={'p99_ms':2 if peer=='extensible' else 1})
                rows.append(r)
        result=verdicts.analyze(rows)
        self.assertTrue(result['costs'])
        self.assertTrue(all(r['winner']=='inconclusive' and not r['response_mix_matched'] for r in result['costs']))


class RefinementTest(unittest.TestCase):
    def test_binary_refinement_stops_at_resolution_without_fixed_four_levels(self):
        peers=['baseline','head','extensible']
        rows=[probe(v,rate) for rate in (45000,60000) for v in peers]
        added=[]
        for _ in range(3):
            decision=capacity.next_probe(rows,peers,5000)
            if decision is None: break
            added.append(decision['rate'])
            rows += [probe(v,decision['rate']) for v in peers]
        self.assertEqual(added,[52500,56000])
        bounds=capacity.probe_bounds(rows,peers)['head']
        self.assertLessEqual(bounds['upper']-bounds['lower'],5000)
        self.assertFalse(capacity.next_probe(rows,peers,5000))
        status=capacity.search_status(rows,peers,5000,len(added),3)
        self.assertTrue(status['head_probe_resolution_met'])
        self.assertFalse(status['budget_exhausted'])

    def test_budget_exhaustion_does_not_claim_requested_resolution(self):
        rows=[probe(v,rate) for rate in (45000,60000) for v in ('baseline','head')]
        status=capacity.search_status(rows,['baseline','head'],5000,0,0)
        self.assertTrue(status['budget_exhausted'])
        self.assertFalse(status['head_probe_resolution_met'])
        self.assertEqual(status['head_probe_gap_rps'],15000)

    def test_invalid_or_generator_limited_probes_do_not_form_upper_bound(self):
        for changes in ({'errors':1},{'loadgen_cpu_pct':190}):
            rows=[probe('baseline',rate) for rate in (10000,16000)]
            rows += [probe('head',10000),probe('head',16000,limit=12000,**changes)]
            b=capacity.probe_bounds(rows,['baseline','head'])['head']
            self.assertIsNone(b['upper'])

    def test_confirmation_uses_two_head_endpoints_not_every_peer_rate(self):
        peers=['baseline','head','extensible']
        rows=[probe(v,rate,limit=20000 if v=='head' else 10000) for rate in (10000,20000,30000) for v in peers]
        plan,details=campaign.confirmation_plan(rows,'redis',peers)
        self.assertEqual([p['rate'] for p in plan],[20000,30000])
        self.assertEqual(details['common_probe_rate'],10000)
        self.assertTrue(all(p['variants']==peers for p in plan))

    def test_five_clean_confirmations_establish_passing_failing_bracket(self):
        rows=[probe(v,rate,limit=50000,repeat=i) for rate in (50000,52500)
              for i in range(5) for v in ('baseline','head')]
        b=next(b for b in verdicts.analyze(rows)['boundaries'] if b['variant']=='head')
        self.assertEqual((b['highest_pass'],b['lowest_fail'],b['gap']),(50000,52500,2500))
        self.assertEqual(b['state'],'bracketed')

    def test_non_monotonic_outcomes_do_not_claim_stable_interval(self):
        rows=[probe('baseline',rate) for rate in (10000,20000)]
        rows += [probe('head',10000,limit=5000),probe('head',20000)]
        plan,details=campaign.confirmation_plan(rows,'redis',['baseline','head'])
        self.assertFalse(plan)
        self.assertEqual(details['probe_bounds']['head']['state'],'non-monotonic')
        self.assertIsNone(capacity.next_probe(rows,['baseline','head'],2000))

    def test_campaign_plan_has_bounded_work_and_explicit_full_matrix(self):
        cli=Path(campaign.__file__)
        def plan(*args): return json.loads(subprocess.check_output([sys.executable,str(cli),'--backend','redis','--output','unused','--plan',*args],text=True))
        default=plan();full=plan('--full-matrix')
        self.assertEqual(default['probe_rates'],[10000,16000])
        self.assertEqual(default['resolution_rps'],2000)
        self.assertLess(default['max_confirmation_rounds'],full['max_confirmation_rounds'])
        self.assertEqual(plan('--check')['refinement_budget'],0)

    def test_runner_plan_previews_the_actual_positive_rejection_quota(self):
        data=json.loads(subprocess.check_output([sys.executable,str(Path(bench.__file__)),'compare',
            '--backends','memory','--variants','head','governor','--clients','100','--modes','reject',
            '--rates','10000','--duration','2','--plan'],text=True))
        self.assertEqual(data['quotas'],[dict(backend='memory',rate=10000,clients=100,mode='reject',quota=2)])


class BuildReuseTest(unittest.TestCase):
    def test_only_unchanged_source_settings_and_exact_images_are_reused(self):
        metadata=dict(git_sha='sha',source_hashes={'src':'digest'},rust='1.98.0',build='release --locked',http_keep_alive_seconds=120)
        images={'head':dict(Id='image',Architecture='arm64',Os='linux')}
        source={**metadata,'images':images}
        bench.verify_reuse(metadata,source,images)
        for key in ('git_sha','source_hashes','build'):
            changed=copy.deepcopy(metadata);changed[key]='changed'
            with self.subTest(key=key),self.assertRaises(RuntimeError): bench.verify_reuse(changed,source,images)
        for key in ('Id','Architecture','Os'):
            changed=copy.deepcopy(images);changed['head'][key]='changed'
            with self.subTest(key=key),self.assertRaises(RuntimeError): bench.verify_reuse(metadata,source,changed)
