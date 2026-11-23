import tempfile
import unittest
from pathlib import Path
from upstream_compat import ANCHORS, select_tag, probe, gate


class UpstreamTests(unittest.TestCase):
    def test_stable_numeric_selection(self):
        entries = [(t, s) for t, s in [("v0.1.9", "a"), ("v0.1.10", "b"), ("v0.2.0-rc.1", "c")]]
        sample = "\n".join(s * 40 + "\trefs/tags/" + t for t, s in entries)
        self.assertEqual(select_tag(sample), ("v0.1.10", "b" * 40))

    def test_prerelease_only_is_rejected(self):
        with self.assertRaises(ValueError):
            select_tag("a" * 40 + "\trefs/tags/v0.1.17-rc.1")

    def test_anchor_and_dependency_drift_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, anchors in ANCHORS.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("\n".join(anchors) + (
                    '\ngpui = { package = "gpui-pre", version = "=0.3.8" }'
                    if name == "Cargo.toml" else ""))
            adapter = root / "adapter.toml"
            adapter.write_text('gpui = { package = "gpui-pre", version = "=0.3.8" }\n')
            good = probe(root, adapter, "v0.1.16", "a" * 40)
            self.assertTrue(gate(good, adapter.read_text()))
            (root / "src/commands.rs").write_text("pub const COMMANDS")
            bad = probe(root, adapter, "v0.1.17", "b" * 40)
            self.assertFalse(gate(bad, adapter.read_text()))
            (root / "Cargo.toml").write_text('gpui = { package = "gpui-pre", version = "=0.3.9" }\n')
            self.assertFalse(probe(root, adapter, "v0.1.17", "b" * 40)["compatible"])
            good["adapter_gpui"] = "4.0"
            self.assertFalse(gate(good, adapter.read_text()))


if __name__ == "__main__":
    unittest.main()
