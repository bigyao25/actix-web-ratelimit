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
    def test_expanded_ci_smoke_has_thirteen_cells(self):
        cmd=[sys.executable,str(CLI),'smoke','--resources','ci','--variants','baseline','head','v0.1.2','v0.2.1','governor','trypema','extensible','--plan']
        plan=json.loads(subprocess.check_output(cmd,text=True))
        self.assertEqual(plan['rounds'],13)

    def test_pk_defaults_are_high_load_and_dedicated(self):
        plan=json.loads(subprocess.check_output([sys.executable,str(CLI),'pk','--plan'],text=True))
        self.assertEqual(plan['rounds'],135)
        self.assertEqual(plan['rates'],[10000,30000,60000])
        self.assertEqual(plan['load_seconds'],3375)

    def test_pk_does_not_use_shared_smoke_resources(self):
        result=subprocess.run([sys.executable,str(CLI),'pk','--resources','ci','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)

    def test_pk_hard_ceiling_is_bounded(self):
        result=subprocess.run([sys.executable,str(CLI),'pk','--rates','100001','--plan'],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('100000',result.stderr)
