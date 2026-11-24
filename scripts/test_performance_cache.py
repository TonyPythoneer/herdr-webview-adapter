import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import performance as p
import performance_cache as c
import performance_feedback as feedback

class ContentCacheTests(unittest.TestCase):
    def test_fixture_content_and_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, hit = c.fixture(root, "fixture.html", b"immutable")
            self.assertFalse(hit)
            same, hit = c.fixture(root, "fixture.html", b"immutable")
            self.assertEqual(same, path)
            self.assertTrue(hit)
            changed, hit = c.fixture(root, "fixture.html", b"new")
            self.assertFalse(hit)
            self.assertNotEqual(path, changed)
            path.write_bytes(b"corrupt")
            repaired, hit = c.fixture(root, "fixture.html", b"immutable")
            self.assertFalse(hit)
            self.assertEqual(repaired.read_bytes(), b"immutable")
            with self.assertRaises(ValueError):
                c.fixture(root, "../escape", b"x")

    def test_exact_binary_cache_and_invalidation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree = root / "tree"
            tree.mkdir()
            lock = tree / "Cargo.lock"
            lock.write_text("initial lock")
            store = root / "store"
            fixture, _ = c.fixture(store, "workload.rs", b"workload")
            fixtures = {"workload.rs": fixture}
            identity = {"image_os": "macos15", "image_version": "v1", "rust": "1.95"}
            def fake_build(tree, native, target, destination, fixtures):
                destination.mkdir()
                path = destination / "perf_fixed"
                path.write_bytes(b"real fake binary")
                path.chmod(0o755)
                return {"perf_fixed": path}
            with patch.object(p, "build", side_effect=fake_build) as build:
                def get(name, rev="revision1", env=identity):
                    return p.build_cached(tree, rev, False, root/"target", root/name, store, fixtures, env, lock)
                first, status, key = get("first")
                self.assertEqual(status, "miss")
                second, status, same = get("second")
                self.assertEqual(status, "hit")
                self.assertEqual(key, same)
                self.assertEqual(c.digest(first["perf_fixed"]), c.digest(second["perf_fixed"]))
                self.assertEqual(build.call_count, 1)
                self.assertEqual(get("new_revision", rev="revision2")[1], "miss")
                self.assertEqual(get("new_environment", env=dict(identity, image_version="v2"))[1], "miss")
                lock.write_text("updated lock")
                self.assertEqual(get("new_lock")[1], "miss")
                lock.write_text("initial lock")
                fixtures["workload.rs"], _ = c.fixture(store, "workload.rs", b"changed")
                self.assertEqual(get("new_fixture")[1], "miss")
                fixtures["workload.rs"] = fixture
                cached = store/"binaries"/key/"perf_fixed"
                cached.write_bytes(b"tampered")
                self.assertEqual(get("corrupt")[1], "miss")
                cached.chmod(0o644)
                self.assertEqual(get("not_executable")[1], "miss")
                self.assertEqual(get("unknown_image", env={})[1], "disabled_unknown_environment")

    def test_cache_lru_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root/"perf_fixed"
            binary.write_bytes(b"data")
            binary.chmod(0o755)
            for i in range(10):
                key = c.content_key({"i":i})
                c.save(root, key, {"perf_fixed": binary})
                os.utime(root/"binaries"/key, (i+1, i+1))
            c.prune(root)
            self.assertEqual(len(list((root/"binaries").iterdir())), c.MAX_ENTRIES)
            self.assertFalse((root/"binaries"/c.content_key({"i":0})).exists())

    def test_feedback_allowlists_cache_data(self):
        data = {"cache":{"binaries":{"baseline":{"status":"hit","key":"@untrusted"},
                                      "candidate":{"status":"@untrusted"}},
                         "fixtures":{"workload.rs":"hit","@untrusted":"miss"}},
                "execution_timing_seconds":{"prepare_and_build":{"baseline":3,"candidate":5,"@untrusted":2},
                                            "measurement":{"portable":1},"profiling":None,
                                            "build_cache_hit":"true"}}
        result = feedback.normalize_ci_cache(data)
        self.assertEqual(result["binaries"], {"baseline":"hit"})
        self.assertEqual(result["fixtures"], {"workload.rs":"hit"})
        self.assertEqual(result["execution_timing_seconds"]["prepare_and_build"], {"baseline":3,"candidate":5})
        self.assertTrue(result["compilation_cache_exact_hit"])
        self.assertNotIn("@untrusted", str(result))
