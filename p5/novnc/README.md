# noVNC asset sourcing and licensing (P5 pre-read)

The view front serves a static tree from `<data>/novnc/` (08, 07) and answers the pinned
`view_unavailable` / `view assets not installed (expected <data>/novnc)` until it exists.
This note pins where those files come from, what they are licensed under, and how to stage
them. Packaging (09) owns the install mechanism; this is the sourcing half.

## Pinned source

**The npm package is not enough.** `@novnc/novnc@1.7.0` ships only the core library
(`core/`, `vendor/`, licence docs, `AUTHORS`) — its `files` list excludes `vnc.html` and
`app/`, so a tree installed from npm cannot satisfy the front's `vnc.html` lookup. The runnable
tree is the upstream **source** tarball at the release tag:

| | |
|---|---|
| Project | noVNC (`github.com/novnc/noVNC`) |
| Tag | **v1.7.0** (annotated tag `9e7def8…`, commit `63107bd06d9e1f6136ff21aeda8cd62cbf0d433e`) |
| Tarball | `https://codeload.github.com/novnc/noVNC/tar.gz/refs/tags/v1.7.0` |
| SHA-256 | `b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6` |
| Source tree | 2.9 MiB unpacked; staged subset (below) 1.6 MiB, 129 files |

Pin the **artifact hash** and the tag commit, not `latest` or a moving branch: a re-generated
archive must fail closed and force a deliberate pin update. (Upstream publishes tags only — no
release assets — so codeload is the canonical artifact.)

## Licence matrix (this is not a single-licence drop)

`LICENSE.txt` splits the distribution (verified in the 1.7.0 tarball):

- **Core JS** (`core/**`, `app/*.js`, `test/playback.js`) — **MPL-2.0**, file-level copyleft.
  Shipping the files unmodified is fine; include the licence and keep the source available
  (the pinned upstream URL satisfies this). **Do not patch core files** without publishing the
  patch under MPL-2.0 and recording it — that is the one obligation that changes our paperwork.
- **HTML / CSS** (`*.html`, `app/styles/*.css`) — **BSD-2-Clause**.
- **Fonts** (`app/styles/Orbitron*`) — **SIL OFL-1.1** (Copyright 2009 Matt McInerney).
- **Images** (`app/images/**`) — **CC-BY-SA-3.0**: attribution required, and the *images* carry
  share-alike. Keep the licence text and attribution with them.
- **`vendor/pako`** — MIT (its own `LICENSE` ships in the tree).

The npm tarball's licence metadata (`"license": "MPL-2.0"`) is the *core* licence only; a
packaging row that records one SPDX id for the whole asset tree would be wrong.

Practical consequence: the tree must ship with `LICENSE.txt`, `docs/LICENSE.MPL-2.0`,
`docs/LICENSE.OFL-1.1`, `docs/LICENSE.BSD-2-Clause`, `docs/LICENSE.BSD-3-Clause` and
`vendor/pako/LICENSE` present. Dropping "just the JS" is not licence-clean.

## Staging layout

`<data>/novnc/` is the asset root the front uses (`Paths::novnc_dir`, `views::ASSETS_DIR`); the
front serves `vnc.html` (falling back to `index.html`) and refuses path traversal. Stage the
tarball's `package/` **contents at the root**, not nested:

```
<data>/novnc/
  vnc.html
  app/            # app/*.js, styles/, images/
  core/           # core/**/*.js
  vendor/pako/    # MIT, licence inside
  docs/           # LICENSE.MPL-2.0, .OFL-1.1, .BSD-2-Clause, .BSD-3-Clause
  LICENSE.txt
  AUTHORS
```

Stage into a sibling temp dir and `rename(2)` into place, so a half-extracted tree can never be
served; the front already fails closed (503) while the tree is absent.

## Fetch recipe

The mechanism now ships with packaging, where 09 assigns install: `packaging/novnc/fetch.sh
[dest] [tarball]` downloads the pinned source tarball, checks the SHA-256, stages the
**subset the front needs** (`vnc.html`, `app/`, `core/`, `vendor/`, `LICENSE.txt`, `AUTHORS`,
`docs/LICENSE.*`) and asserts the entry points exist. It takes no credentials and does no
network calls beyond the pinned URL; an offline host passes the tarball as the second argument
and gets the same hash check. `vnc_lite.html`, `tests/`, `utils/`, `po/`, `snap/` and the CI
files are deliberately not staged. This note stays the sourcing record: the pin, the licence
split, and why the npm package cannot serve the front.

## Open items for packaging (09)

- Whether the release manifest carries the noVNC tarball hash as its own row (recommended), or
  the installer re-derives it from the core release manifest.
- Whether `docs/` licence files are copied into `<data>/novnc/docs/` or a single
  `LICENSES/` summary is generated at install time. Either is compliant; the current tree keeps
  them in `docs/`.
- `vnc.html` references a small set of `app/images/**`; excluding the CC-BY-SA images to avoid
  the share-alike obligation is *not* recommended (broken chrome), so the notices ship instead.
