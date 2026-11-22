# Local B2S test fixtures (not committed)

Copied from `/mnt/pinball/Backups/Tables` and `/mnt/pinball/B2Spro` for manual
testing of `vpxtool b2s ...`. Do not commit; the files are 25-340 MB each.

## b2s-testdata/ (directb2s)

| fixture | what it tests |
|---|---|
| `Attack from Mars (Bally 1995) 2.0.1` vpx+directb2s | grill 373, authored `<DMD>` tag 484,1240 1034x218, hole in the grill band, no vbs sidecar (rom from embedded script: `afm_113b`) |
| `Attack from Mars (Bally 1995)` vpx+directb2s | same `(Bally 1995)` key as the two below → *similar/ambiguous* name matching |
| `Attack from Mars (Bally 1995) g5k 1.1.vpx` (no own b2s) | falls back to the `g5k 1.1p` file by the year key |
| `Attack from Mars (Bally 1995) g5k 1.1p` vbs+vpx | vbs sidecar takes precedence over the embedded script |
| `Attack from Mars (Bally 1995) g5k 1.1p.directb2s` | 47-byte stub file → read-failure must be a per-table note, not a crash |
| `AC_DC (LUCI Premium) (Stern 2013) LUCI` vbs+vpx+directb2s | grill 632, tag 616,2060 1264x316, sidecar vbs (`acd_170h`) |
| `Transformers (Pro) (Stern 2011)` vpx+directb2s | no sidecar, rom only in the embedded script; grill 750, tag 699,2145 1596x399 |
| `1-2-3 (Automaticos 1973)` vpx+directb2s | `GrillHeight 0` → must be left untouched by every method |
| `AC_DC (Pro) (Stern 2012) Pro` vbs+vpx, no b2s | missing-backglass report |
| `b2spro/NBA Fastbreak (Bally 1997)` vpx + two `.B2SPro` | case-insensitive extension, two similar names → ambiguous, shortest name wins |
| `b2spro/MIBS (Gottlieb 1969).B2SPro` | B2SPro root is also `DirectB2SData`; grill 0 |
| `b2spro/Terminator 3 (Stern 2003) DMDmarker.B2SPro` + `Terminator 3 (Stern 2003) Siggi-TW VLM mod.vpx` | similar-name match across vpx/b2s stems; authored tag from a DMD marker pass |

## DmdDevice.ini

Use `dmdconverter/DmdDevice.ini` (stock template) plus the community ini
sections of the design notes; per-ROM sections used by the fixtures:
`[afm_113b]`, `[acd_170h]`, `[trn_177tb]`.
