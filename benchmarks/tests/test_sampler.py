"""Streaming resource samples must remain usable JSON after terminal cleanup."""
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0,str(Path(__file__).parents[1]))
import bench


class SamplerTest(unittest.TestCase):
    def test_stream_controls_and_blank_frames_are_removed(self):
        sample='{"CPUPerc":"1.2%","MemUsage":"2MiB / 1GiB"}'
        raw='\x1b[2J\x1b[H'+sample+'\x1b[K\n\x1b[K\n\x1b[J'+sample+'\n'
        samples=bench.stats_samples(raw)
        self.assertEqual(len(samples),2)
        self.assertTrue(all(json.loads(s)['CPUPerc']=='1.2%' for s in samples))

    def test_empty_and_failed_sampler_are_rejected(self):
        for raw in ('\x1b[K\n','Cannot connect to Docker daemon\n','{}\n'):
            with self.subTest(raw=raw),self.assertRaises((RuntimeError,ValueError)):
                bench.stats_samples(raw)
