import tempfile
import unittest
from pathlib import Path
from upstream_compat import ANCHORS, select_tag, probe, gate
from coordinated_deps import eligible_core, select_wry, rewrite_manifest, host_core, adapter_wry_pin


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
            good = probe(root, adapter, "v0.1.16", "a" * 40, "a" * 40)
            self.assertTrue(gate(good, adapter.read_text()))
            (root / "src/commands.rs").write_text("pub const COMMANDS")
            bad = probe(root, adapter, "v0.1.17", "b" * 40, "b" * 40)
            self.assertFalse(gate(bad, adapter.read_text()))
            (root / "Cargo.toml").write_text('gpui = { package = "gpui-pre", version = "=0.3.9" }\n')
            self.assertFalse(probe(root, adapter, "v0.1.17", "b" * 40, "b" * 40)["compatible"])
            good["adapter_gpui"] = "4.0"
            self.assertFalse(gate(good, adapter.read_text()))
            good["adapter_gpui"] = "0.3.8"
            good["tag_object_sha"] = "invalid"
            self.assertFalse(gate(good, adapter.read_text()))

    def test_annotated_tags_keep_object_and_commit_identities_separate(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, anchors in ANCHORS.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("\n".join(anchors) + (
                    '\ngpui = { package = "gpui-pre", version = "=0.3.8" }'
                    if name == "Cargo.toml" else ""))
            manifest = root / "adapter.toml"
            manifest.write_text('gpui = { package = "gpui-pre", version = "=0.3.8" }')
            report = probe(root, manifest, "v0.1.17", "a" * 40, "b" * 40)
            self.assertEqual(report["sha"], "a" * 40)
            self.assertEqual(report["tag_object_sha"], "b" * 40)
            self.assertTrue(gate(report, manifest.read_text()))



class CoordinatedDependencyTests(unittest.TestCase):
    def test_no_upgrade_until_both_upstream_and_host_advance(self):
        self.assertFalse(eligible_core("0.3.8", "0.3.8", "0.3.8"))
        self.assertFalse(eligible_core("0.3.8", "0.3.9", "0.3.8"))
        self.assertFalse(eligible_core("0.3.9", "0.3.8", "0.3.8"))
        self.assertTrue(eligible_core("0.3.8", "0.3.9", "0.3.9"))

    def test_registry_selection_rejects_incompatible_yanked_and_api_upgrades(self):
        def record(version, core, yanked=False):
            return {"vers": version, "yanked": yanked, "deps": [
                {"name": "gpui", "package": "gpui-pre", "req": core, "kind": "normal"}]}
        records = [record("0.7.3", "=0.3.9"), record("0.7.4", "=0.3.10"),
                   record("0.7.5", "=0.3.9", True), record("0.8.0", "=0.3.9"),
                   record("0.7.6", "^0.3.9"), record("0.7.7-rc.1", "=0.3.9")]
        self.assertIsNone(select_wry(records, "0.7.2", "0.3.8"))
        self.assertEqual(select_wry(records, "0.7.2", "0.3.9"), "0.7.3")
        self.assertEqual(select_wry(records, "0.7.2", "0.3.10"), "0.7.4")
        self.assertIsNone(select_wry(records, "0.7.4", "0.3.9"))

    def test_manifest_keeps_coupled_types_in_sync(self):
        manifest = """
[target.'cfg(target_os = "macos")'.dependencies]
gpui = { package = "gpui-pre", version = "=0.3.8" }
gpui-wry = "=0.7.2"
wry = { package = "lb-wry", version = "=0.53.3" }
[target.'cfg(target_os = "macos")'.dev-dependencies]
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.8", features = ["test-support"] }
"""
        result = rewrite_manifest(manifest, "0.3.9", "0.7.3")
        self.assertEqual(host_core(result), "0.3.9")
        self.assertEqual(adapter_wry_pin(result), "0.7.3")
        self.assertIn('version = "=0.53.3"', result)
        with self.assertRaises(ValueError):
            host_core(manifest.replace('version = "=0.3.8", features', 'version = "=0.3.9", features'))


if __name__ == "__main__":
    unittest.main()
