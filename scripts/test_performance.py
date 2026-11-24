import unittest
import performance as p

class ComparisonTests(unittest.TestCase):
    def test_quantiles(self):
        self.assertEqual(p.summarize(list(range(1,101))), {"n":100,"p50":50,"p95":95,"p99":99})
    def pair(self, base, candidate):
        return {"baseline":{"samples":{"metric":[base]},"meta":{}},"candidate":{"samples":{"metric":[candidate]},"meta":{}}}
    def test_improvement(self):
        self.assertEqual(p.compare([self.pair(100,80)]*p.PAIRS,"test")[0]["classification"],"improvement signal")
    def test_regression(self):
        self.assertEqual(p.compare([self.pair(100,120)]*p.PAIRS,"test")[0]["classification"],"regression signal")
    def test_inconclusive(self):
        self.assertEqual(p.compare([self.pair(100,80)]*4+[self.pair(100,101)],"test")[0]["classification"],"inconclusive")
    def test_incomplete(self):
        with self.assertRaises(RuntimeError):
            p.compare([self.pair(100,80)],"test")
    def test_different_browser(self):
        pair=self.pair(100,80)
        pair["candidate"]["meta"]={"chrome_version":"different"}
        with self.assertRaises(RuntimeError):
            p.compare([pair]*p.PAIRS,"test")

class NativeProofTests(unittest.TestCase):
    def test_pixel_missing_final_proof_fails(self):
        with self.assertRaisesRegex(RuntimeError, "WK_PIXEL_VERIFIED"):
            p.require_native_proof("wk_native_pixels", "native WKWebView screenshot pixel test passed")
    def test_pixel_complete_proof(self):
        p.require_native_proof("wk_native_pixels", "native WKWebView screenshot pixel test passed\nWK_PIXEL_VERIFIED:")
    def test_input_missing_proof_fails(self):
        with self.assertRaisesRegex(RuntimeError, "WK_TRUSTED_WHEEL_VERIFIED"):
            p.require_native_proof("wk_native_input", "WK_TRUSTED_INPUT_VERIFIED:")
    def test_input_complete_proof(self):
        p.require_native_proof("wk_native_input", "WK_TRUSTED_KEY_VERIFIED:\nWK_TRUSTED_TEXT_VERIFIED:\nWK_TRUSTED_WHEEL_VERIFIED:\nWK_TRUSTED_INPUT_VERIFIED:")
