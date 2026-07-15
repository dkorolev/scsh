#!/usr/bin/env python3

import re
import sys
from collections import Counter


def top_words(text):
    words = re.findall(r"[a-z]+", text.lower())
    return Counter(words).most_common(10)


def main():
    for word, count in top_words(sys.stdin.read()):
        print(f"{word}\t{count}")


if __name__ == "__main__":
    main()
