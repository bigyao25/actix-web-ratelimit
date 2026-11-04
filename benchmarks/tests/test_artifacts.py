"""Cache cleanup must preserve source, reports and symlink targets."""
from contextlib import redirect_stdout
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('artifacts',Path(__file__).parents[1]/'artifacts.py')
artifacts=importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifacts)

class ArtifactsTest(unittest.TestCase):
    def test_cleanup_preserves_results_and_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp).resolve()
            target=root/'adapters'/'head'/'target'
            target.mkdir(parents=True)
            (target/'binary').write_text('generated')
            report=root/'results'/'raw.json'
            report.parent.mkdir()
            report.write_text('{}')
            source=target.parent/'Cargo.lock'
            source.write_text('locked')
            with patch.object(artifacts,'HERE',root),patch('sys.argv',['artifacts.py','--clean-local-cache']),redirect_stdout(io.StringIO()):
                artifacts.main()
            self.assertFalse(target.exists())
            self.assertTrue(report.exists())
            self.assertEqual(source.read_text(),'locked')

    def test_cleanup_refuses_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp).resolve()
            (root/'adapters'/'head').mkdir(parents=True)
            outside=root/'preserve'
            outside.mkdir()
            (root/'adapters'/'head'/'target').symlink_to(outside,target_is_directory=True)
            with patch.object(artifacts,'HERE',root),patch('sys.argv',['artifacts.py','--clean-local-cache']),self.assertRaises(SystemExit):
                artifacts.main()
            self.assertTrue(outside.exists())
