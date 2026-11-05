"""Capability and evidence rules; fixtures are not benchmark data."""
import sys
from pathlib import Path
import unittest
sys.path.insert(0,str(Path(__file__).parents[1]))
import comparison
import variants

class CompetitorTest(unittest.TestCase):
    def test_capabilities_do_not_silently_use_fake_zero_quotas(self):
        self.assertFalse(variants.supports('governor','redis','allow'))
        self.assertFalse(variants.supports('trypema','memory','deny'))
        self.assertTrue(variants.supports('extensible','redis','deny'))
        self.assertTrue(variants.supports('trypema','redis','mixed'))

    def test_single_round_never_gets_separation_label(self):
        base=dict(platform_id='host',scenario='memory/test',target_rps=100,p99_ms=1,variant='head')
        peer={**base,'variant':'extensible','algorithm':'fixed window','p99_ms':10}
        result=comparison.contrasts([base,peer])
        self.assertIn('fewer than five',result[0]['evidence'])
        self.assertEqual(result[0]['p99_ratio'],10)

    def test_matched_resource_limits_are_required(self):
        base=dict(platform_id='host',scenario='memory/test',target_rps=100,p99_ms=1,variant='head',server_cpu_limit=2)
        peer={**base,'variant':'trypema','server_cpu_limit':1}
        self.assertFalse(comparison.contrasts([base,peer]))
