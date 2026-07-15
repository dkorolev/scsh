import unittest

from wordstats import top_words


class WordStatsTests(unittest.TestCase):
    def test_counts_words_case_insensitively(self):
        self.assertEqual(top_words("Pear apple pear"), [("pear", 2), ("apple", 1)])


if __name__ == "__main__":
    unittest.main()
