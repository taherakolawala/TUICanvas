import unittest

from tools.browser_bench import description_snippet, spread, summarize


class BrowserBenchTests(unittest.TestCase):
    def test_snippet_is_visible_text_only(self):
        html = "<style>p{}</style><p>Implement the <b>tiled</b>\n matrix multiply from lecture.</p><script>secret()</script>"
        self.assertEqual(description_snippet(html), "Implement the tiled matrix multiply from")
        self.assertEqual(description_snippet(None), "")

    def test_long_snippet_is_cut_at_a_word(self):
        snippet = description_snippet("<p>" + "abcdefghij " * 12 + "</p>")
        self.assertLessEqual(len(snippet), 60)
        self.assertFalse(snippet.endswith(" "))

    def test_spread_and_failed_runs(self):
        self.assertEqual(spread([3, 1, 2]), {"median": 2, "min": 1, "max": 3})
        self.assertIsNone(spread([]))
        ok = {"step": "dashboard", "visible_ms": 100, "wall_ms": 110, "html_response_end_ms": 50, "dom_content_loaded_ms": 80,
              "load_event_ms": 90, "requests_until_visible": 10, "bytes_until_visible": 1000, "requests_total": 12, "bytes_total": 1200}
        runs = [{"condition": "warm cache", "run": 1, "steps": [ok]},
                {"condition": "warm cache", "run": 2, "steps": [{"step": "dashboard", "error": "TimeoutError"}]}]
        summary = summarize(runs)["warm cache"]["steps"]["dashboard"]
        self.assertEqual(summary["visible_ms"]["median"], 100)
        self.assertEqual(summary["failed_runs"], 1)


if __name__ == "__main__":
    unittest.main()
