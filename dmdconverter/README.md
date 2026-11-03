# DMDConverter

Writes the DMD rectangle into B2S backglass files:

```xml
<DirectB2SData>
  ...
  <Images>
    <BackglassImage P="…" Value="…base64…"/>
  </Images>
  <Displays>
    <DMD Target="Backglass" X="484" Y="1240" W="1034" H="218" GlassRoughness="0.1"/>
  </Displays>
</DirectB2SData>
```

B2S uses the `<DMD>` element to know where the DMD cut-out of a backglass is.
Most files in the wild do not have it, so backglass packs fall back to a
default position. This tool finds the rectangle and writes it, in place, for a
whole folder tree.

## Build

```sh
cargo build --release
```

## Use

Point it at the folder that holds the tables. Nothing is written without
`--write`.

```sh
# report only
target\release\dmdconverter.exe scan D:\vPinball\VisualPinball\Tables

# stamp, with a .bak next to every changed file
target\release\dmdconverter.exe scan D:\vPinball\VisualPinball\Tables --write
```

One table per line, with the numbers that went into the decision:

```
Attack from Mars (Bally 1995) 2.0.1 v78.2.vpx
  b2s   Attack from Mars (Bally 1995) 2.0.1 v78.2.directb2s
  rom   afm_113b  (embedded)
  bg    2000x1565
  grill 373
  tag   484,1240 1034x218  aspect 4.74
  hole  504,1287 949x164  aspect 5.79
  ini   3174,2310 1322x384  aspect 3.44  ->  480,1255 1033x209 in image
  =>    504,1287 949x164 via hole
```

## Where the rectangle comes from

`--method hole` (the default) detects the black cut-out in the backglass image
that is embedded in the file. It needs nothing besides the backglass file
itself, which is the point: a pack can be stamped without an installation.

A detected blob is only a DMD if it could be one. Two filters apply before a
rectangle is written:

- **Shape and place.** The cut-out is 2:1–6:1, at least 6 % of the image high,
  starts left of 60 % of the width, sits in the bottom 55 %, and is 25–90 % of
  the width. Flat art bands are 6.2–8.9:1 and 4.7–7.4 % high; the 244
  DMD-shaped tags of one installation are 2.4–5.9:1 and 7–25 % high (median
  13 %). A raw 128x16 DMD is 8:1, but the window cut into a backglass is not.
- **DMD evidence.** The backglass must show that it has a DMD at all: a
  `DMDImage`, a valid authored `<DMD>` tag, or a `[cGameName]` section in
  `DmdDevice.ini`. Without one, a plausible rectangle is a score panel or a
  band of artwork, and the file is left untouched.

What is left is ranked by distance to the position the authoring tools draw
(x ≈ 0.242·W, y ≈ 0.81·H, w ≈ 0.51·W, 4:1), and the strip of `GrillHeight` at the
bottom of the image wins over the rest of the image.

`GrillHeight` must be a real strip: at least 10 px and at least 1 % of the image
height. 1–5 px values occur in the wild, but they are leftover settings, not a
speaker panel, and no stamp is made on them.

`--method ini` reads `virtualdmd left/top/width/height` from the `[cGameName]`
section of `DmdDevice.ini`. Those are desktop coordinates, so they are mapped
into image pixels through the backglass monitor (see [Geometry](#geometry)).
Use it when a table has no detectable hole.

`--method auto` prefers the hole and falls back to the ini.

`--method template` adds the last resorts: first the `<DMD>` tag the author
already wrote, then the authored box itself, computed from the backglass size.
Nothing is invented out of thin air — the numbers are the median of the files
that do carry a tag — and the box is only used where the backglass has a real
grill strip, shows DMD evidence (`DMDImage`, tag or ini section), and the tracer
saw no flat rectangle at all. A backglass with no grill and no DMD window has no
evidence of where a DMD belongs, so the file is left untouched. The report and
the JSON say which of the four sources each table used
(`hole`, `ini`, `tag`, `template`).

The ROM name that keys the ini lookup is read from the table's script, the
`.vbs` sidecar first, then the script embedded in the `.vpx`. The XML
`GameName` is not used: it names the table (`afm_113`), not the ROM
(`afm_113b`).

Only a real assignment counts: `'` and `Rem` comments are stripped (also after
code on the same line), the `cGameName` must be a whole identifier (the
`B2ScGameName = "..."` DOF/backglass name does not match), and a name inside a
string literal is ignored.

## Aspect

The three sources do not agree on the shape, and the difference is bigger than
rounding: on one installation the glass hole is 4.74:1, the authored box 4:1,
and the ini window 3.44:1. So the ratio is a switch, not a derivation.

| `--aspect` | what it stamps |
|---|---|
| `hole` (default) | the detected hole, glass border included, as the authoring tools do |
| `native` | keeps the width, derives the height from `--native-grid`: `128x32` (WPC), `192x64` (SAM), `4x1` (the plain content box) |
| `ini` | the ini window's own ratio |

`--native-grid` is a switch because resolving it per ROM needs the ROM
database, which this tool deliberately does not depend on.

## Geometry

The ini window is in desktop pixels, the `<DMD>` tag is in backglass image
pixels, and the backglass image covers the backglass monitor. So:

```
scale  = image width / backglass monitor width
tagX   = (ini left - monitor left) * scale
tagY   = ini top * image height / monitor height
tagW   = ini width * scale
tagH   = ini height * image height / monitor height
```

`--backglass-monitor 2560,0,2560,2880` is the default: a 2560x2880 portrait
backglass monitor to the right of a 2560x1440 playfield monitor, both at 100 %
scale, desktop coordinates 2560..5120. Change it to match your cabinet, in the
pixels Windows reports for that monitor (its resolution at 100 % scale).

Fitted against the seven authored tags of one installation: the x lands within
a median of 1 px, the top within 15 px, the width within 3 %. The height comes
out 11 % to 16 % too tall, because the ini window includes the frame VPinMAME
draws around the DMD. That is what `--aspect native` fixes.

## Writing

`--write` edits the file in place. The splice is surgical: everything outside
the `<DMD>` element, the base64 payloads included, stays byte identical. By
default the original is kept as `<name>.directb2s.bak`; an existing `.bak` is
never overwritten, so the first state the tool saw is what can be restored.
`--no-backup` skips that. A file whose tag already has the right numbers is not
touched: the comparison uses the whole pixels that are written, so a second run
writes nothing and leaves the timestamp alone. Several tables can match the same
backglass; it is stamped once, and an exact name match wins over a similar one.

## Options

```
scan <ROOT>
  --ini <PATH>                 DmdDevice.ini, defaults to DmdDevice.ini in the
                               current folder, then the VPinMAME locations
  --method <hole|ini|auto|template>
                               how to derive the rectangle
  --aspect <hole|native|ini>   ratio of the stamped rectangle
  --native-grid <WxH>          dot grid for --aspect native
  --backglass-monitor <X,Y,W,H> backglass monitor in desktop pixels
  --no-hole                    skip image decoding, ini only (much faster)
  --glass-roughness <F>        GlassRoughness to write, default 0.1
  --json <PATH>                dump every record for analysis
  --write                      write the files
  --no-backup                  no .bak
  --limit <N>                  stop after N tables
  --jobs <N>                   tables at a time, default: all cores
```

Reading a table means decoding a 20-50 MB base64 PNG and parsing the XML around
it, so `--jobs` pays: 1096 tables of one installation take 104 s on 12 cores.
The workers share a budget of in-flight megabytes (peak of 3.5 GB seen), so 12
cores do not ask for 100 GB at once. A damaged
`.vpx` (`vpin` panics on a missing table of contents) becomes a note on that
table and does not stop the run.

## Whole installation

One tree, 1096 tables, `--method template` (dry run):

| | |
|---|---|
| b2s file found | 949 tables match 931 files (848 exact name, 85 similar, 16 ambiguous), 147 tables have none |
| ROM name from the script | 943, of which 349 have a `DmdDevice.ini` section |
| rect from the hole | 186 table matches, 182 files |
| rect from the ini | 141 table matches, 137 files |
| rect from the author's tag | 2 table matches, 2 files |
| rect from the template (grill strip) | 54 table matches, 54 files |
| no rect / no b2s | 713 table matches (556 files with a backglass, 147 tables without) |
| stampable | **375 files of 931** with a backglass (383 table matches, 8 share a file) |
| files that already carry a `<DMD>` | 261; 227 already correct, 17 replaced, 17 left alone |
| write plan | 227 unchanged, 17 replace, 131 insert (148 files changed) |

## Matching

`Foo Pro (Stern 2011).vpx` matches `Foo Pro (Stern 2011).directb2s` exactly,
and `Foo Pro (Stern 2011) v2.directb2s` by the "same up to and including
`(Company Year)`" rule. Only `.directb2s` and `.b2spro` are considered, case
insensitively. `.directb2st`, `.directb2sss`, `.directb2s_no` and friends are
typos, leftovers, and split-pack backups; the tool reports them as missing
rather than guessing.

## Tests

```sh
cargo test
```
