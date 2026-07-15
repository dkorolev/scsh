# Word statistics

`wordstats.py` reads text from standard input and prints the ten most common words as
tab-separated `word` and `count` rows.

```sh
printf 'Pear apple pear\n' | python3 wordstats.py
```

Run the tests with:

```sh
python3 -m unittest -v
```
