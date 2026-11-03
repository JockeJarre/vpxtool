# DMDConverter — design notes

Companion to `PLAN.md` (the request) and `README.md` (how to use it). This file
records what has been **verified on real files** in this installation, and why
the code is the way it is. Anything marked *unconfirmed* is a hypothesis that
still needs data.

## Goal

Scan a folder tree for `*.vpx`, find the matching B2S backglass file
(`.directb2s` / `.b2spro`), and stamp the DMD rectangle
(`<DirectB2SData><Displays><DMD Target="Backglass" X Y W H GlassRoughness/>`) so
the backglass server knows where the DMD cut-out is inside the backglass image.

## Verified facts

### File naming in a real installation

| file | note |
|---|---|
| `AC-DC LUCI Premium VR (Stern 2013) v1.1.4.vpx` | + same-name `.vbs` sidecar, `.directb2s`, `.info` |
| `Attack from Mars (Bally 1995) 2.0.1 v78.2.vpx` | + `.vbs`, `.directb2s`, table `.ini` |
| `Transformers Pro (Stern 2011) v.2.4.1.vpx` | `.directb2s`, **no** `.vbs` sidecar → the rom name is only in the embedded script |

Whole `Tables` tree (recursively): 1096 `.vpx`, 1051 `.directb2s`/`.b2spro`,
plus B2S-Pro files with **mixed-case and look-alike extensions**: `.B2SPro`,
`.directb2sPRO`, `.directb2st`, `.directb2sss`, `.directb2s_no`. Matching is
case-insensitive, and only the two documented extensions are candidates.

### The B2S XML

| table | BackglassImage | DMDImage | GrillHeight | `<DMD>` tag |
|---|---|---|---|---|
| Attack from Mars | 2000x1565 | – | 373 | X=484 Y=1240 W=1034 H=218 |
| AC-DC LUCI | 2500x2472 | 1920x1080 | 632 | X=616 Y=2060 W=1264 H=316 |
| Transformers Pro | 3000x2653 | 1920x1080 | 750 | X=699 Y=2145 W=1596 H=399 |

- `BackglassImage@Value` / `DMDImage@Value` are plain base64 **PNG**, with CRLF
  inside the attribute → strip whitespace before decoding.
- `GrillHeight` is the height of the grill strip at the **bottom** of the
  backglass image. 243 of the 245 `<DMD>` tags that have a grill and an image
  size lie inside `[image_height - GrillHeight, image_height]`, and 208 of the
  211 detected holes that have a grill height lie there too, which is why the
  picker prefers that band and uses it to reject flat colour up in the
  playfield art.
- The tag as a *fraction* of the image is stable across authors: across the
  244 DMD-shaped tags, x/W is 0.21-0.26 (median 0.24), w/W 0.48-0.58 (median
  0.51), y/H 0.77-0.85 (median 0.81).
- 261 of the 931 backglass files of one installation carry a `<DMD>` tag; 246
  of them are 2.4-5.9:1, the other 15 are 6.0-8.9:1 stripes at the top edge
  that no DMD window is. Many tags are the hole rectangle exactly (140 of the
  147 files that have both), so a hole-based tool placed them, not a person.
- The structural tail of a file is LF (`</Images>\n    <Displays>\n` …) while the
  base64 payloads above contain CRLF, so the line ending for an inserted block
  is detected **after `</Images>`**, not from the whole file.
- `.b2spro` has the same tags as `.directb2s` (the B2S-Pro sources read them the
  same way), so one reader handles both and the stamp goes in place.

### `GameName` in the XML is **not** the ROM

| table | XML `GameName@Value` | VBS `cGameName` | DmdDevice.ini section |
|---|---|---|---|
| Attack from Mars | `afm_113` | `afm_113b` | `[afm_113b]` |
| AC-DC LUCI | `acd_165` | `acd_170h` | `[acd_170h]` |

So the ROM name comes from the script: sidecar `.vbs` if present, else the
script embedded in the `.vpx`. `DMDType` / `DMDDefaultLocation` are B2S
authoring fields and carry no usable position.

### `DmdDevice.ini` (source: `pinmame/dmddevice/dmddevice.cpp`)

- Section = ROM name, **exact** (case-sensitive) match; `[global]` is merged in
  first, later duplicate sections override.
- Keys per section: `virtualdmd left|top|width|height` (+ `enabled`, `style`,
  `position x|y|width|height` for the DMDext window).
- Units: **physical screen pixels, desktop coordinates** (`SetWindowPos` on the
  backglass monitor). Floats are accepted.
- In B2S mode pinmame *recentres* the DMD inside the B2S window
  (`nLeft = left - width/2 + B2SBackbufferWidth/2`) and sends
  `WM_VPINCMD "setconfig b2s x y w h"`.
- `resize = fit|fill|stretch` in `[global]` changes the sent geometry.
- The installed `DmdDevice.ini` on this machine is the stock template (only
  `[global]`, `[virtualdmd]`, `[pindmd*]`): 0 per-ROM sections. The per-ROM
  sections come from the community ini, so `--ini` matters and the default looks
  in the current folder first.

### ini window → backglass image (the mapping, fitted)

`virtualdmd left/top/width/height` is where the DMDext window sits on the
desktop; the `<DMD>` tag is in backglass image pixels. The backglass image
covers the backglass monitor, so:

```
scale = image width / monitor width
tagX  = (ini left - monitor left) * scale
tagY  = ini top * image height / monitor height
tagW  = ini width * scale
tagH  = ini height * image height / monitor height
```

Both sides are physical pixels, so DPI scaling cancels. Cabinet of this
installation: playfield 2560x1440 (100 %, x 0) left, backglass 2560x2880
portrait (100 % scale, desktop x 2560..5120) right →
`--backglass-monitor 2560,0,2560,2880` is the default.

Fitted over the six files that have both an authored tag and an ini section:

| table | authored tag | mapped ini | dx | dy | dw | dh |
|---|---|---|---|---|---|---|
| Attack from Mars | 484,1240 1034x218 | 480,1255 1033x209 | −4 | +15 | 0 % | −4 % |
| AC-DC LUCI | 616,2060 1264x316 | 617,2039 1268x367 | +1 | −21 | 0 % | +16 % |
| Transformers Pro | 699,2145 1596x399 | 712,2133 1573x461 | +14 | −12 | −1 % | +15 % |
| Cactus Canyon | 580,1740 1369x238 | 591,1663 1324x269 | +11 | −77 | −3 % | +13 % |
| Terminator 3 | 335,1005 589x183 | 256,1025 751x203 | −79 | +20 | +27 % | +11 % |

x, y and width land within a few pixels (median dx +1, dy +15, dw 3 %). The
height is consistently 11–16 % too tall, because the ini window is a *window*:
DMDext draws a title bar and frame around the DMD content
(`dmddevice.cpp:1993,2558,3470` for the styles). Terminator 3 is the outlier —
its authored box is 3.22:1 and does not correspond to its ini window.

### Why the ratio is a switch

Three sources, three ratios, differences far beyond rounding: AFM's glass hole
is 4.74:1, the authored box 4.74 (the author traced the hole), AC-DC's authored
box 4.00, the ini window 3.44. The DMD *content* is 4:1 (128x32 WPC) or 3:1
(192x64 SAM). So `--aspect` selects a policy; the default keeps the hole, which
is what authoring tools produce.

### The hole detector, over the whole installation

The detector is vpxtool's algorithm: split the image into a grid, seed every
cell whose neighbourhood is uniform, ray-trace left/right/up/down and the four
diagonals, keep rectangles of aspect 2–9. Raw tracing over the 942 readable
backglasses of one installation found 878 flat rectangles in 291 files; 59 of
those files have no candidate that passes the window, so they are not
stamped:

| table | detected | what it is |
|---|---|---|
| Cirqus Voltaire (VPW Mod) | `0,2276 2987x620` | the whole bottom strip, art included |
| Elvis (Stern 2004) | `419,0 2165x257` | a light panel at the **top** edge |
| Big House (Gottlieb 1989) | `1290,5 850x327` | the dark right half of the artwork |

So a candidate has to be plausible before it is a stamp: `x ≤ 0.60·W` (a DMD
flush with the left edge hides behind the bezel), `y` in the bottom 55 % of the
image, `w` between 25 % and 90 % of the width, `w ≥ 5 %` (a 4:1 box that wide is
a line, not a DMD), aspect 2–6:1, and at least 6 % of the image high. The shape
bounds separate the DMD windows that authors did place (3.2–5.8:1, 11–15 % high)
from the flat bands of art that the tracer finds on backglasses of games without
a DMD (6.2–8.9:1, 4.7–7.4 % high). The bounds are fractions, so they scale with
the author's image size. `hole::is_plausible` is that window; `pick_hole` filters
by it, prefers the grill band, and then ranks by distance to the authored
template (x 0.242·W, y 0.81·H, w 0.51·W, 4:1) rather than by size — the flat
areas of the art are usually **bigger** than the real cut-out, so "largest" is
the wrong ranking (the Cirqus Voltaire band is).

A plausible hole is still not enough. The file has to say that the backglass has
a DMD: a `DMDImage`, a valid authored tag, or a `[cGameName]` section in
`DmdDevice.ini`. Without one of those, the rectangle is a score display or a
band of artwork (Banzai Run's lower panel, Heavy Metal's score window), and the
file is left untouched. A grill strip is required for **every** stamp, and it
has to be a real one: at least 10 px and at least 1 % of the image height. 1–5 px
`GrillHeight` values are leftovers, not speaker panels.

228 files have a candidate that passes the window. 147 of the 261 tagged files
also have an accepted hole; 140 of those pairs are the same rectangle, and the
remaining nine pairs (six distinct layouts) are the hand-placed ones:

| table | authored tag | hole | dx | dy | dw | dh |
|---|---|---|---|---|---|---|
| Game of Thrones | 591,2034 1317x273 | 592,2034 1299x279 | +1 | 0 | −1 % | +2 % |
| Ghostbusters LE | 624,2029 1246x269 | 644,2044 1195x238 | +20 | +15 | −4 % | −12 % |
| KISS | 703,2455 1586x396 | 717,2446 1564x419 | +14 | −9 | −1 % | +6 % |
| Terminator 3 | 335,1005 589x183 | 340,1003 572x185 | +5 | −2 | −3 % | +1 % |
| AC-DC LUCI | 616,2060 1264x316 | 621,2041 1253x353 | +5 | −19 | −1 % | +12 % |
| Attack from Mars | 484,1240 1034x218 | 504,1287 949x164 | +20 | +47 | −8 % | −25 % |

The x lands 1–20 px to the right (median 14), the width is 1–8 % narrower. The
detected box is the cut-out; the author's box is what he drew around it, so the
two differ by that border (median dy 0, dw −3 %, dh +1 %). Attack from Mars is
the largest gap, and the only file where the author traced the whole grill
recess including its frame.

## How to test

```sh
cargo test                                             # 50 unit tests
cargo run --release -- scan Tables\X9 --ini DmdDevice.ini --json x9.json
cargo run --release -- scan D:\vPinball\VisualPinball\Tables --ini DmdDevice.ini \
  --method template --json scan-final.json                                      # 104 s on 12 cores
cargo run --release -- scan <tree> --no-hole --method ini --json ini.json        # ini only, no image decode
```

The full-library run is the one to read: 1096 tables in 104 s at `--jobs` = 12
(default `available_parallelism`), peak of 3545 MB in flight, and it is a dry run
unless `--write` is given.

`scan` without `--write` is read-only: it prints one block per table, writes the
JSON, and reports the stamp plan as a dry run.

| line | meaning |
|---|---|
| `rom` | `cGameName` from the sidecar `.vbs` / embedded script, and whether `DmdDevice.ini` has a section |
| `bg` / `dmdimg` | embedded image sizes |
| `grill` | `GrillHeight@Value` |
| `tag` | the `<DMD>` rect already in the file (the author's value) |
| `hole` | detected cut-out in the backglass image |
| `dmdhl` | detected cut-out in the grill strip (`DMDImage`) — diagnosis only, its coordinates are **not** backglass space |
| `ini` | the ini window, desktop pixels, `-> ` the same window in image pixels |
| `=>` | what would be stamped, and from which source |

### Stamping, on copies

```sh
Copy-Item <tree>\X.directb2s $env:TEMP\opencode\stamp-test\    # plus an empty X.vpx
cargo run --release -- scan $env:TEMP\opencode\stamp-test --ini DmdDevice.ini --write
```

Verified on the 284-file Testbench copy set: the splice changes only the tag
bytes (prefix and suffix of the six largest 47–61 MB files compared
byte-for-byte), all 19 `.bak` files are byte identical to the pre-state, the
265 files that were left untouched keep both their hash and their timestamp,
the 15 insert paths write a `<Displays>` block that `.NET XmlReader` parses with
the expected `X/Y/W/H/GlassRoughness` (116 files parse, 0 errors, 0 numbers
different from the plan), and a second run reports `unchanged 116`,
`written 0`, with no file changed on disk.

Over the whole installation with `--method template` (dry run): 383 table
matches get a rect, covering 375 files (8 tables share a backglass); 227 of
those files already hold the right rectangle, 17 are `replace` (a different
authored tag), 131 `insert`. The other 556 files with a backglass have no
acceptable rect (no real grill, no DMD evidence, or nothing DMD-shaped), so
they are reported as `no rect` and left untouched; 147 tables have no
backglass file at all. A full `--write` run therefore rewrites 148 files
(4.7 GB) and keeps 4.7 GB of `.bak` copies, so `--no-backup` is worth it once
the tool is trusted.

## Stamping methods

1. **`hole` (default)** — decode the embedded backglass image, find the
   uniform-colour cut-out, write it. Needs nothing besides the backglass file:
   no installation, no ini, no ROM. Reference: vpxtool's `find_hole` in
   `D:\git\Flipper\vpxtool\src\backglass.rs` (grid of seeds, ray-trace in 4
   directions + 4 diagonals, largest region wins), used there by
   `auto_position_dmd` (`src\frontend.rs:708`) for the inverse direction. This
   tool adds the plausibility window and the template-distance ranking described
   above. Measured against the 7 authored tags it can see: dx +5, dy −2, dw −1 %,
   dh +6 % (median).
2. **`ini`** — the ini window mapped into the image (see the fit above).
3. **`auto`** — hole first, ini as fallback: the ini rect is desktop pixels, the
   hole is already in the tag's space.
4. **`template`** — hole, ini, the `<DMD>` tag the author already wrote, and only
   then the authored box computed from the image size (`hole::template_rect`:
   x 0.242·W, y 0.81·H, w 0.51·W, 4:1). The box needs a real grill strip, DMD
   evidence (a `DMDImage`, a tag or an ini section) and an image in which the
   tracer saw no flat rectangle at all; otherwise the file is left untouched
   instead of being given a guessed rectangle. The numbers of the template are
   the median of the files that do carry a tag. A `0,0 0x0` placeholder does not
   count as an authored tag, and a tag whose shape fails the plausibility window
   is not evidence either: an 8.9:1 stripe an older run wrote is ignored.


The source of every rect is in the report (`via hole|ini|tag|template`) and in
the JSON as `source`, because a stamped file whose numbers came from the template
is a different statement than one where a hole was found.

## Idempotence and untouched files

`plan_bytes` decides `unchanged` by comparing the rectangle *as it is written*
(`{:.0}`), not the raw floats: the hole detector returns fractions, the file
holds whole pixels, and a raw compare rewrote identical bytes on every run (and
churned the `.bak`). Two consequences are checked on every run:

- a file that already holds the tag is not opened for writing, so its timestamp
  and its `.bak` stay as they were;
- several `.vpx` tables can match the same backglass; each file is stamped once,
  and the exact name match wins over a similar one, so the second table does not
  rewrite the tag the first one just inserted.

An existing `.bak` is never overwritten, so the first state the tool saw is the
one that can be restored, even if a later run with other options changes the
file again.

The template fallback above is the other half of "do not touch a file with
nothing to update": with `--method template`, 375 of the 931 files with a
backglass get a rect, and the 556 with no real grill, no DMD evidence, or no
DMD-shaped rectangle are reported as `no rect` and left alone.

## Pipeline

1. Walk the tree for `*.vpx` (optionally `.vpz` packs — vpin can read the script
   from `vpz::read_info(path)?.script`).
2. Match the backglass file: exact stem, else the "same up to and including the
   `(Company Year)`" rule, case-insensitive extension.
3. Stream-parse the XML (`GrillHeight`, `Displays/DMD`, `Images/BackglassImage`,
   `Images/DMDImage`, `DMDType`, `DualBackglass`) — never a DOM over 12–55 MB.
4. ROM name: sidecar `.vbs` first, else the embedded script
   (`vpin::vpx::open(p)?.read_gamedata()?.code.string`), per-line regex,
   skipping `'` comments, unifying lone-CR endings.
5. `DmdDevice.ini`: `[global]` + `[rom]` merge, case-insensitive keys.
6. Rect: hole, ini (mapped), authored tag, template (real grill + DMD evidence
   + no rejected flat rectangle); ratio by `--aspect`.
7. Write: touch only the `<DMD>` element, create `<Displays>` when missing
   (including `<Displays/>` and `<Displays></Displays>`), keep the rest of the
   document byte-identical. `--write` opt-in, `.tmp` + rename, `.bak` next to
   the file.
8. Report per table, and skip with a reason: no b2s file, no rom, grill height
   0 or too small for a real strip, no DMD evidence (no `DMDImage`, tag or ini
   section), no hole candidate, a rejected flat stripe, already stamped,
   ambiguous name match.
9. The tables are independent, so they are read on `--jobs` threads over one
   shared index counter, gated by a budget of in-flight megabytes estimated from
   the file sizes. `vpin` **panics** on a `.vpx` without a central directory
   ("the table of contents is not available"), which a real collection always
   contains, so one table's read is wrapped in `catch_unwind` and becomes
   a note; `main` installs a one-line panic hook so the payload prints once.

## Stack

- Rust, single binary, in this folder.
- `vpin` 0.39.1 from crates.io for `.vpx` CFB + `GameStg/GameData` script
  reading (this is what vpxtool itself uses).
- `quick-xml` 0.41 for streaming XML, `image` for PNG decode.
- Custom ini reader: the file has quoted values, duplicate sections,
  case-insensitive keys, `;` comments.

## Files

| file | role |
|---|---|
| `main.rs` | CLI, `Method`, `Aspect`, `Grid`, `Rect`, ini candidates |
| `scan.rs` | tree walk, name matching, per-table collection, `pick`, ini mapping, scan pool + byte budget, panic containment |
| `b2s.rs` | streaming XML reader |
| `rom.rs` | `cGameName` from `.vbs` sidecar / embedded script |
| `dmdini.rs` | `DmdDevice.ini` reader |
| `hole.rs` | vpxtool `find_hole` port, grill-band picker, plausibility window, template rect |
| `stamp.rs` | plan / splice / write, `.bak`, stats |
| `report.rs` | per-table report, summary |

## Reference code

| what | where |
|---|---|
| VBS script from .vpx, `cGameName` regex, sidecar precedence | `D:\git\Flipper\vpxtool\src\indexer.rs:718,1424,1574` |
| DMD hole detection | `D:\git\Flipper\vpxtool\src\backglass.rs:106` |
| B2S image → screen window (inverse direction) | `D:\git\Flipper\vpxtool\src\frontend.rs:708` |
| DmdDevice.ini parsing | `D:\git\Flipper\pinmame\dmddevice\dmddevice.cpp:1993,2558,3470` |
| ini → screen placement, B2S recentering | `dmddevice.cpp:5057,5081,3758,4513` |
| B2S XML schema | `D:\git\Flipper\B2S-Pro\Backglass Server\Model\*.vb` |

## Open decisions

1. Hole filters: done as described above (shape window + DMD evidence + real
   grill + grill band + template ranking + `--method template` fallback). What is
   left is quality, not coverage: backglasses with no DMD at all are now left
   untouched by design. The remaining question is whether a file with an
   alphanumeric display but no DMD evidence should ever get a tag, and that is a
   user decision, not a detector fix.
2. Native grid per rom: needs the rom database (`vpin-mame`'s
   `Game::from_rom(..).dmd_resolution()`), which is the vpxtool integration, not
   a dependency here.
3. `GlassRoughness`: 0.1 is the B2S-Pro sample value; is there a preferred
   default per table type?
4. Fixture policy: the real `.directb2s` files are 25–55 MB — the unit tests use
   tiny synthetic XML; keep it that way.
5. Upstream: `rust-directb2s` has no `Displays`/`DMD`, and its reader drops the
   B2S Pro `Bulb`/`Images` attributes too — branch `feat/displays-dmd`
   (`%TEMP%\opencode\rust-directb2s`) models all of it, the issue text is
   `%TEMP%\opencode\issue-rust-directb2s.md`. vpxtool has no stamp command, its
   `find_hole` has no plausibility filter, and the ini window has no documented
   mapping into image space. Nothing is published yet; publishing needs a go-ahead.
