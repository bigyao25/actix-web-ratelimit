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
