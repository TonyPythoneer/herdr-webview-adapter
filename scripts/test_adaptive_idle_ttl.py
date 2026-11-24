import unittest
from adaptive_idle_ttl import adaptive,replay
class Policy(unittest.TestCase):
    def test_bounds(self):
        self.assertEqual(adaptive([0]*8),1)
        self.assertEqual(adaptive([100]*8),15)
        self.assertEqual(adaptive([]),5)
    def test_no_lookahead(self):
        a=replay([1,2,100],adaptive)["decisions"]
        b=replay([1,2,0],adaptive)["decisions"]
        self.assertEqual([x["ttl_s"] for x in a],[x["ttl_s"] for x in b])
    def test_cost(self):
        r=replay([1,5,10],lambda _:5)
        self.assertEqual((r["hits"],r["misses"],r["idle_process_seconds"]),(1,2,11))
    def test_recent_window(self):
        self.assertEqual(adaptive([100]*30+[1]*8),1)
