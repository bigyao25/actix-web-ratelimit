"""CLI planning must validate bounds without accessing Docker."""
import json
from pathlib import Path
import subprocess
import sys
import unittest

CLI=Path(__file__).parents[1]/'bench.py'

class PlanTest(unittest.TestCase):
    def test_rate_sweep_plan_counts_cells(self):
        command=[sys.executable,str(CLI),'safe','--variants','baseline','head',
                 '--backends','memory','--clients','1','--concurrency','32',
                 '--modes','allow','--rates','100','1000','2000','--plan']
        output=subprocess.check_output(command,text=True)
        plan=json.loads(output)
        self.assertEqual(plan['rounds'],30)
        self.assertEqual(plan['load_seconds'],2250)

    def test_plan_rejects_rate_above_safe_cap(self):
        result=subprocess.run([sys.executable,str(CLI),'safe','--rates','2001','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('1..2000',result.stderr)

    def test_explore_has_higher_bounded_compact_matrix(self):
        output=subprocess.check_output([sys.executable,str(CLI),'explore','--plan'],text=True)
        plan=json.loads(output)
        self.assertEqual(plan['rates'],[1000,2000,4000,8000,10000])
        self.assertEqual(plan['rounds'],20)
        self.assertEqual(plan['load_seconds'],240)

    def test_ci_layout_is_not_a_capacity_profile(self):
        result=subprocess.run([sys.executable,str(CLI),'explore','--resources','ci','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)


class CompetitorPlanTest(unittest.TestCase):
    def test_smoke_excludes_historical_performance_duplicates(self):
        cmd=[sys.executable,str(CLI),'smoke','--resources','ci','--variants','baseline','head','governor','trypema','extensible','--plan']
        plan=json.loads(subprocess.check_output(cmd,text=True))
        self.assertEqual(plan['rounds'],9)

    def test_comparison_defaults_are_high_load_and_dedicated(self):
        plan=json.loads(subprocess.check_output([sys.executable,str(CLI),'compare','--plan'],text=True))
        self.assertEqual(plan['rounds'],110)
        self.assertEqual(plan['rates_by_backend'],{'memory':[30000,60000],'redis':[15000,20000,25000]})
        self.assertEqual(plan['load_seconds'],3850)
        self.assertEqual(plan['concurrency'],[512])
        self.assertEqual(plan['repeats'],5)

    def test_comparison_does_not_use_shared_smoke_resources(self):
        result=subprocess.run([sys.executable,str(CLI),'compare','--resources','ci','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)

    def test_comparison_hard_ceiling_is_bounded(self):
        result=subprocess.run([sys.executable,str(CLI),'compare','--rates','100001','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('100000',result.stderr)

    def test_legacy_is_a_one_shot_defect_proof(self):
        plan=json.loads(subprocess.check_output([sys.executable,str(CLI),'defect','--resources','ci','--plan'],text=True))
        self.assertEqual(plan['rounds'],1)
        self.assertEqual(plan['load_seconds'],1)
        result=subprocess.run([sys.executable,str(CLI),'compare','--variants','v0.1.2','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('one-shot defect',result.stderr)
