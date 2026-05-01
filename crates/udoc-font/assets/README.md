# Bundled font assets

These fonts ship inside `udoc-font` (and are re-used by `udoc-render`)
as the fallback set when a PDF references a standard font without
embedding the program, or when no embedded glyph is available for a
requested character. They are selected for broad Unicode coverage at the
smallest reasonable binary cost.

Each entry below lists the upstream source, version pin, subset range
(where applicable), license, and the final on-disk size. Licenses live
in `LICENSES/`.

## Tier 0: core fallbacks (shipped from day one)

| File                              |    Size | Source                                                                       | Version  | License                    |
|-----------------------------------|--------:|------------------------------------------------------------------------------|----------|----------------------------|
| `LiberationSans-Regular.ttf`      |  401 KB | <https://github.com/liberationfonts/liberation-fonts>                        | 2.1.5 (*)| SIL Open Font License 1.1  |
| `LiberationSerif-Regular.ttf`     |  385 KB | <https://github.com/liberationfonts/liberation-fonts>                        | 2.1.5 (*)| SIL Open Font License 1.1  |
| `NotoSansCJK-Subset.cff_bundle`   | 2056 KB | <https://github.com/notofonts/noto-cjk> (subsetted + bundled via custom tool) | 2.004    | SIL Open Font License 1.1  |

(*) The Sans/Serif Regular binaries predate the Liberation 2.1.5 tarball
drop we use for the Tier 1 weights; their bytes are preserved to avoid
disturbing existing goldens. Tier 1 faces below come from the 2.1.5
release.

## Tier 1: weight + monospace + LaTeX fallbacks (M-35, ;, )

| File                              |   Size | Source                                                                                              | Version / pin                                                    | Subset                                                    | License              |
|-----------------------------------|-------:|-----------------------------------------------------------------------------------------------------|------------------------------------------------------------------|-----------------------------------------------------------|----------------------|
| `LiberationSans-Bold.ttf`         | 405 KB | `https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz` | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationSans-Italic.ttf`       | 406 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationSans-BoldItalic.ttf`   | 400 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationSerif-Bold.ttf`        | 362 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationSerif-Italic.ttf`      | 367 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationSerif-BoldItalic.ttf`  | 368 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LiberationMono-Regular.ttf`      | 312 KB | same tarball                                                                                        | 2.1.5                                                            | none (full face)                                          | OFL 1.1              |
| `LatinModernRoman-Regular.otf`    |  94 KB | `https://www.gust.org.pl/projects/e-foundry/latin-modern/download/Latin_Modern-otf-2_007-31_03_2026.zip` (file `lmroman10-regular.otf`) | 2.007 (2026-03-31)                                               | none; the upstream optical-size-10 face is already narrow | GUST Font License    |
| `LatinModernRoman-Italic.otf`     | 106 KB | same zip, file `lmroman10-italic.otf`                                                               | 2.007 (2026-03-31)                                               | none (full face)                                          | GUST Font License    |
| `LatinModernMath-Subset.otf`      | 218 KB | `https://www.gust.org.pl/projects/e-foundry/lm-math/download/latinmodern-math-1959.zip`             | 1.959 (2014-09-05, still the latest upstream release at the time of M-35) | see below                                                 | GUST Font License    |

The three Liberation Serif weights (Bold, Italic, BoldItalic) were added
in  under (GitHub #193) so that `Times-Bold`,
`Times-Italic`, and `Times-BoldItalic` references route to a real weight
instead of falling back to `LiberationSerif-Regular` with synthetic
stem-widening. They are pinned to the same Liberation 2.1.5 release as
the Sans weights.

Tier 1 total: **approximately 3.0 MB** (was ~1.9 MB before).
Combined with Tier 0 the bundle is roughly **5.8 MB**. A CI guardrail
(`tier1_size_budget`) keeps the Tier 1+2 bytes under 4.0 MB so any future
asset change surfaces visibly.

## Tier 2: script-coverage fallbacks ()

| File                              |   Size | Source                                                                                              | Version / pin                      | Subset                  | License              |
|-----------------------------------|-------:|-----------------------------------------------------------------------------------------------------|------------------------------------|-------------------------|----------------------|
| `NotoSansArabic-Regular.ttf`      | 230 KB | `https://github.com/notofonts/arabic/releases/download/NotoSansArabic-v2.013/NotoSansArabic-v2.013.zip` (file `NotoSansArabic/hinted/ttf/NotoSansArabic-Regular.ttf`) | NotoSansArabic v2.013 (2025-10-15) | none (full hinted face) | OFL 1.1              |
| `NotoSansArabic-Bold.ttf`         | 255 KB | same zip, file `NotoSansArabic/hinted/ttf/NotoSansArabic-Bold.ttf`                                  | NotoSansArabic v2.013 (2025-10-15) | none (full hinted face) | OFL 1.1              |

Tier 2 Arabic total: **approximately 485 KB**. Routes via
`route_by_unicode` for Arabic / Arabic Supplement / Arabic Presentation
Forms-A / Arabic Presentation Forms-B (U+0600..06FF, U+0750..077F,
U+FB50..FDFF, U+FE70..FEFF) and via `route_tier1` for explicit Arabic
font names (`NotoSansArabic`, `NotoNaskhArabic`, `Amiri`, `Scheherazade`,
etc.). Closes the IA-spanish-Arabic genre on the stratified-100 bench:
the Liberation fallbacks have zero Arabic coverage so without these
faces the failure mode is `.notdef` boxes for every Arabic glyph.

Feature gate: `tier2-arabic` (default ON). Disable for slim builds that
target Latin/CJK-only corpora.

### `LatinModernMath-Subset.otf`

Subsetted with `pyftsubset` (fontTools 4.62.1). Unicode ranges kept:

| Block                                      | Range              | Purpose                                             |
|--------------------------------------------|--------------------|-----------------------------------------------------|
| Basic Latin                                | `U+0020-007F`      | ASCII letters/digits used as math operands           |
| Latin-1 Supplement                         | `U+00A0-00FF`      | Non-breaking space, degrees, super/sub-script digits |
| Greek and Coptic                           | `U+0370-03FF`      | Greek variables used throughout LaTeX                |
| Mathematical Operators                     | `U+2200-22FF`      | forall, exists, integral, sum, nabla, ...            |
| Miscellaneous Mathematical Symbols-A       | `U+27C0-27EF`      | Angle brackets, small triangles, etc.                |
| Miscellaneous Mathematical Symbols-B       | `U+2980-29FF`      | Additional brackets, fences                          |
| Supplemental Mathematical Operators        | `U+2A00-2AFF`      | Big operators                                        |
| Mathematical Alphanumeric Symbols          | `U+1D400-1D7FF`    | Bold/italic/script/sans/mono letter variants         |

Invocation:

```
pyftsubset latinmodern-math.otf \
  --unicodes=U+0020-007F,U+00A0-00FF,U+0370-03FF,\
U+2200-22FF,U+27C0-27EF,U+2980-29FF,U+2A00-2AFF,U+1D400-1D7FF \
  --output-file=LatinModernMath-Subset.otf \
  --glyph-names --symbol-cmap --notdef-glyph --recommended-glyphs \
  --drop-tables+=DSIG,MATH --no-layout-closure
```

`MATH` is dropped because `udoc-font` does not consume it; `DSIG` is
dropped because it is meaningless after subsetting. Layout closure is
skipped to keep the size near ~220 KB. The resulting face still carries
`GPOS`/`GSUB` for basic shaping, 1468 glyphs, and full coverage of the
retained ranges.

`LatinModernRoman-*.otf` are shipped as-is: the optical-size-10 design
already weighs in at under 110 KB per face and covers Latin,
Latin-1 Supplement, and nearly all of Latin Extended-A, which is the
span pdfTeX emits for Western typography.

## Licenses

| License                                    | File                                      | Applies to                                              |
|--------------------------------------------|-------------------------------------------|---------------------------------------------------------|
| SIL Open Font License 1.1                  | `LICENSES/Liberation-OFL.txt`             | Liberation Sans/Serif/Mono                              |
| Liberation font authors notice             | `LICENSES/Liberation-AUTHORS.txt`         | Liberation attribution                                  |
| SIL Open Font License 1.1                  | `LICENSES/Liberation-OFL.txt` (shared)    | Noto Sans CJK SC (same license text)                    |
| GUST Font License                          | `LICENSES/GUST-FONT-LICENSE.txt`          | Latin Modern Roman, Latin Modern Math                   |
| SIL Open Font License 1.1                  | `LICENSES/NotoSansArabic-OFL.txt`         | Noto Sans Arabic Regular + Bold                         |
| Noto Sans Arabic authors notice            | `LICENSES/NotoSansArabic-AUTHORS.txt`     | Noto Sans Arabic attribution                            |

The GUST Font License is an LPPL-based permissive license; it allows
verbatim redistribution and modification under name-change conditions.
We do not rename the faces because we ship them verbatim.

## Re-subsetting / refreshing

If you regenerate `LatinModernMath-Subset.otf`, keep the unicode list
and the `--drop-tables` set above so the size budget and golden tests
stay stable. If you bump Liberation from 2.1.5 to a newer tag, pin the
exact release URL in this file and expect all 40+ per-glyph goldens to
need a re-bless.
