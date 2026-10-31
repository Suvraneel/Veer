# Fonts

Geist and Geist Mono 1.9.0 by Vercel and basement.studio, from the `geist`
npm package, under the SIL Open Font License 1.1 (`OFL.txt`). They're served
from here rather than Google Fonts so the page has no render-blocking request
to another site.

Each is the variable font (weights 100 to 900) cut down to Latin and the
punctuation the pages use:

```sh
U="U+0020-007E,U+00A0-00FF,U+0131,U+0152-0153,U+02C6,U+02DA,U+02DC,U+2010-2027,U+2030-203A,U+2044,U+20AC,U+2122,U+2190-2199,U+21C4,U+21E7,U+2212,U+2303,U+2318,U+2325,U+2713,U+2715"
pyftsubset Geist-Variable.woff2 --unicodes="$U" --layout-features+=tnum --flavor=woff2 --output-file=geist-1.9.0.woff2
pyftsubset GeistMono-Variable.woff2 --unicodes="$U" --layout-features+=tnum --flavor=woff2 --output-file=geist-mono-1.9.0.woff2
```

The version is in the file names because they're cached for a year: a new
version needs new names.
