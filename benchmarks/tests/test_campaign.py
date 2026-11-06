"""Synthetic fixtures validate stage selection, never measured product outcomes."""
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0,str(Path(__file__).parents[1]))
import bench
import campaign
import capacity
import verdicts
import workflow
import test_summary


def row(variant, rate=10000, repeat=0, **changes):
    r=test_summary.SummaryTest().fixture()
    r.update(variant=variant,target_rps=rate,offered_rps=rate,achieved_rps=rate,http_rps=rate,
             scenario=f'memory/1/512/allow/{rate}rps',repeat=repeat,measurement_seconds=30,
             warmup_seconds=5,quota=1000000,window_seconds=95,resource_layout_id='layout',
             http_version='4.18.1',resource_profile='comparison',server_cpu_limit=1,
             loadgen_cpu_limit=2,loadgen_cpu_pct=20,workload='comparison',
             scheduled_latency={'p99_ms':1},server_cpu_pct=40,resource_phase='measurement',
             elapsed_seconds=30.01,resource_capture_skew_seconds=.1)
    r.update(changes)
    return r


class CampaignTest(unittest.TestCase):
    def test_confirmation_keeps_baseline_head_and_adjacent_bounds(self):
        rows=[row(v,rate) for v in ('baseline','head','extensible') for rate in (10000,20000)]
        rows[-1]['http_rps']=10000
        rows[3]['http_rps']=10000
        plan,details=campaign.confirmation_plan(rows,'memory',['baseline','head','extensible'])
        self.assertEqual(plan,[{'rate':10000,'variants':['baseline','head','extensible']},
                               {'rate':20000,'variants':['baseline','head','extensible']}])
        self.assertEqual(details['common_probe_rate'],10000)

    def test_probe_rejects_baseline_failure_and_invalid_peer(self):
        rows=[row('baseline',errors=1),row('head'),row('extensible')]
        self.assertFalse(campaign.confirmation_plan(rows,'memory',['baseline','head','extensible'])[0])

    def test_resource_phase_handshake_brackets_only_measurement(self):
        class Process:
            stdout=io.StringIO('{"event":"measurement_ready"}\n{"event":"measurement_complete"}\n{"http_200":1}\n')
            stdin=io.StringIO()
            returncode=0
            def wait(self): return 0
        p=Process();calls=[]
        def capture(): calls.append(len(calls));return calls[-1]
        result,before,after,seconds=bench.controlled_measurement(p,capture)
        self.assertEqual((before,after),(0,1))
        self.assertEqual(result,{'http_200':1})
        self.assertEqual(p.stdin.getvalue(),'measurement_ready\nmeasurement_complete\n')
        self.assertGreaterEqual(seconds,0)

    def test_round_diagnostic_exposes_generator_pressure(self):
        result=dict(http_rps=56283.79,target_rps=60000,p99_ms=6.89,
                    scheduled_latency={'p99_ms':106.0},loadgen_cpu_pct=198.2,loadgen_cpu_limit=2,
                    server_cpu_pct=80.0,server_cpu_limit=1,dropped_capacity=5950,dropped_late=8918,errors=0)
        line=bench.round_diagnostic(result,60000)
        for token in ('http=56284/60000','sched_p99=106.00ms','loadgen_cpu=198.2%/2','drop_late=8918'):
            self.assertIn(token,line)
        self.assertTrue(bench.generator_pressure(result))
        self.assertFalse(bench.generator_pressure({**result,'dropped_late':0,'loadgen_cpu_pct':100}))
        self.assertIn('sched_p99=n/a',bench.round_diagnostic({'achieved_rps':1},1))

    def test_phase_without_complete_event_is_rejected(self):
        class Process:
            stdout=io.StringIO('{"event":"measurement_ready"}\n')
            stdin=io.StringIO()
            returncode=0
            def wait(self): return 0
        with self.assertRaises(RuntimeError): bench.controlled_measurement(Process(),lambda: {})

    def test_plan_counts_generator_calibration_probes(self):
        output=subprocess.check_output([sys.executable,str(Path(__file__).parents[1]/'campaign.py'),
                                        '--backend','memory','--output','unused','--plan'],text=True)
        plan=json.loads(output)
        self.assertEqual(plan['calibration_probes'],capacity.LOADGEN_STEPS)
        self.assertEqual(plan['loadgen_headroom'],capacity.LOADGEN_HEADROOM)

    def test_collection_rejects_cross_host_or_changed_source_phases(self):
        for changed in ('host','source'):
            with self.subTest(changed=changed),tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp);children=[]
                for i in range(2):
                    child=root/str(i);child.mkdir();children.append((child,'probe' if i==0 else 'confirm'))
                    platform='other' if i and changed=='host' else 'test'
                    hashes={'source':'changed' if i and changed=='source' else 'hash'}
                    (child/'raw.json').write_text(json.dumps({'rounds':[row('head',platform_id=platform)]}))
                    (child/'environment.json').write_text(json.dumps({'git_sha':'sha','source_hashes':hashes}))
                with self.assertRaises(RuntimeError): campaign.collect(root,children)


class CapacityTest(unittest.TestCase):
    def saturated(self, **changes):
        fields=dict(schema_version=3,http_200=285132,sample_count=285132,dropped=14868,
                    dropped_capacity=5950,dropped_late=8918,scheduled=300000,loadgen_saturated=True,
                    error_categories={},achieved_rps=56283.79,http_rps=56283.79,offered_rps=60000,
                    loadgen_cpu_pct=198.2,loadgen_cpu_limit=2,mean_in_flight=50.5,
                    peak_in_flight=512,concurrency_limit=512)
        fields.update(changes)
        return row('baseline',60000,**fields)

    def test_saturated_generator_scales_the_ladder_down(self):
        decision=capacity.ladder_step([45000,60000],[self.saturated()],5000)
        self.assertIsNotNone(decision)
        self.assertEqual(decision['rates'],[30000,40000])
        self.assertLess(decision['rates'][-1],60000)
        self.assertEqual(decision['dropped_late'],8918)

    def test_healthy_generator_keeps_the_configured_ladder(self):
        healthy=self.saturated(dropped=0,dropped_capacity=0,dropped_late=0,scheduled=285132,
                               loadgen_saturated=False,loadgen_cpu_pct=100)
        self.assertIsNone(capacity.ladder_step([45000,60000],[healthy],5000))

    def test_transport_errors_are_not_generator_pressure(self):
        broken=self.saturated(errors=1,error_categories={'timeout':1},http_200=285131)
        self.assertIsNone(capacity.ladder_step([45000,60000],[broken],5000))

    def test_single_rate_ladder_still_scales(self):
        decision=capacity.ladder_step([60000],[self.saturated()],5000)
        self.assertEqual(decision['rates'],[40000])


class VerdictTest(unittest.TestCase):
    def cells(self, **peer_changes):
        return [row(v,repeat=i,**(peer_changes if v=='extensible' else {}))
                for i in range(5) for v in ('baseline','head','extensible')]

    def test_matched_cost_separation_requires_noise_and_intervals(self):
        data=verdicts.analyze(self.cells(scheduled_latency={'p99_ms':2},server_cpu_pct=80))
        self.assertEqual({r['winner'] for r in data['costs']},{'head'})
        self.assertTrue(all(r['pairs']==5 and r['low']>1.05 for r in data['costs']))
        self.assertEqual(data['capacity'][0]['winner'],'inconclusive')

    def test_invalid_or_probe_rows_never_become_winners(self):
        self.assertFalse(verdicts.analyze([dict(r,campaign_stage='probe') for r in self.cells()])['capacity'])
        data=verdicts.analyze(self.cells(errors=1))
        self.assertFalse(data['costs'])
        self.assertEqual(data['capacity'][0]['winner'],'inconclusive')

    def test_clean_confirmed_overload_can_establish_scoped_winner(self):
        data=verdicts.analyze(self.cells(http_rps=9000))
        self.assertEqual(data['capacity'][0]['winner'],'head')
        mixed=self.cells(http_rps=9000)
        mixed[-1]['http_rps']=10000
        self.assertEqual(verdicts.analyze(mixed)['capacity'][0]['winner'],'inconclusive')

    def test_different_hosts_and_unaligned_cpu_do_not_get_cost_winners(self):
        self.assertFalse(verdicts.analyze(self.cells(platform_id='other'))['costs'])
        data=verdicts.analyze(self.cells(server_cpu_pct=80,resource_phase='warmup-and-measurement'))
        self.assertTrue(all(r['winner']=='inconclusive' for r in data['costs'] if r['metric']=='server_cpu_pct'))
        data=verdicts.analyze(self.cells(server_cpu_pct=80,resource_capture_skew_seconds=2))
        self.assertTrue(all(r['winner']=='inconclusive' for r in data['costs'] if r['metric']=='server_cpu_pct'))

    def test_all_allow_rejections_are_not_successful_load(self):
        self.assertFalse(verdicts.passed(row('governor',http_429=1)))

    def test_different_policy_settings_and_short_runs_do_not_establish_winner(self):
        for changes in ({'window_seconds':200,'http_rps':9000},{'measurement_seconds':2}):
            data=verdicts.analyze(self.cells(**changes))
            self.assertEqual(data['capacity'][0]['winner'],'inconclusive')
            self.assertFalse(data['costs'])


class WorkflowTest(unittest.TestCase):
    def test_matrix_shards_backends_not_products_or_repetitions(self):
        self.assertEqual([x['backends'] for x in workflow.matrix('compare')['include']],['memory','redis'])
        self.assertEqual(len(workflow.matrix('smoke')['include']),1)
        self.assertEqual(workflow.matrix('defect')['include'][0]['backends'],'memory')

    def test_aggregation_preserves_independent_hosts_and_detects_missing_shard(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            for name in ('memory','redis'):
                folder=root/('benchmark-diagnostics-'+name);folder.mkdir()
                for filename,data in [('status.json',{'outcome':'success'}),('environment.json',{'git_sha':'sha'}),
                                      ('raw.json',{'rounds':[{'platform_id':name}]}),('verdicts.json',{})]:
                    (folder/filename).write_text(json.dumps(data))
                (folder/'assessment.md').write_text('# Benchmark assessment\n'+name)
            index,content=workflow.aggregate(root,'compare')
            self.assertTrue(index['complete'])
            self.assertEqual([x['platforms'] for x in index['shards']],[['memory'],['redis']])
            (root/'benchmark-diagnostics-redis/status.json').unlink()
            self.assertFalse(workflow.aggregate(root,'compare')[0]['complete'])
