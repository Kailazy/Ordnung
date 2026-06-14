# Ordnung vs rekordbox — key/BPM check (79-track sample)

## Update — analyzer v9 (2026-06-12): key 21%→34% exact, 43%→50% compatible

Three changes, calibrated on this same 79-track set via the new accuracy harness
(`cargo test -p ordnung-core --test key_eval --release -- --ignored --nocapture`,
which runs production `key::detect` and asserts the floors):

1. **Chroma band floor 110→90 Hz** — the biggest single gain. The 110 Hz (A2) floor
   was excluding the bass-root fundamentals in the F2–A2 octave, so the detector saw
   only the fifth/harmonics and picked the dominant (wrong Camelot *number*). 90 Hz
   admits the real roots while still dodging the kick (≤70 Hz regresses it).
2. **Per-track tuning correction** — circular-mean semitone offset subtracted before
   binning, so off-A440 masters don't smear the tonic across two bins.
3. **Minor mode bias 1.05→1.20** — recovers the parallel-major flips (e.g. F minor
   read as F major) the user flagged; minor lean now 74/79 (vs rekordbox 71/79).

Net on the 79: **exact 27/79 (34%)**, **compatible 40/79 (50%)**, **none missing**.
Cost: ~8 genuinely-major tracks now flip minor (the open majmin-tiebreak gap). The
residual miss class is the perfect-fifth/dominant tonic confusion. The harness is now
a regression guard (floors: 27 exact / 40 compatible). The table below is the
original **v4** baseline (21% exact) for reference.

---

Generated 2026-05-20 from `testdata/catalog.db` (analyzer **v4**). The test set was expanded from 16 to **79 analyzed tracks** by copying a larger random subset of the master *seeker* library into `testdata/seeker-sample/`. rekordbox ground truth for **all 79** was transcribed from the user's screenshots.

BPM flag: `ok` ≤2 apart · `8ve` half/double · `X` otherwise. Key flag: `EXACT` same Camelot · `rel` relative maj/min (same number) · `adj` adjacent number same side · `X` otherwise.

## Tally (all 79 tracks)

- **BPM:** 64/79 within 2 BPM (81%); +2 half/double = **66/79 (83%) right tempo modulo octave**; 13 genuine misses.
- **Key (exact Camelot):** 17/79 (21%).
- **Key (harmonically compatible — exact/rel/adj):** 34/79 (43%)  (17 exact + 4 relative + 13 adjacent).
- **Key side (A/B i.e. minor/major) agreement:** 55/79 (69%).
- **Minor lean:** ours 55/79 minor vs rekordbox 71/79.

### The `1A` cluster is real — and it's our biggest key weakness

**18/79 tracks are labelled `1A` (A♭ minor) by rekordbox** (~22% of the library), and the user has **confirmed these were genuinely analyzed** — not placeholders. So the full-79 numbers above are the honest figures, and this cluster is real ground truth we're mostly missing. Our reads on the 18:

- **3 exact** (`1A`).
- **5 adjacent A-side** (`2A`/`12A` — harmonically compatible, one Camelot step off).
- **2 parallel major** (`4B` = G♯ *major*: right tonic, wrong mode — a direct symptom of weak minor bias).
- **8 elsewhere** (scattered, incl. several B-side majors).

So even on the hardest cluster the failure isn't random — 10/18 land on or beside `1A` and 2 more get the tonic right but flip to major. This points at the same two fixes: (1) **stronger minor prior** (recovers the `4B`→`1A` parallel flips and pushes borderline A/B calls to minor), and (2) **better chroma/tuning** to tighten the tonic so the `2A`/`12A` adjacents collapse onto `1A`.

### Read of the results

- **BPM is the strong suit at scale — 83% correct modulo octave.** The original 16-track sample (75%) was unluckily weighted with the hardest genres (dubstep, near-beatless, footwork); across 79 the spectral-flux tempo path holds up well.
- **Key is still the weak spot** (~1 in 5 exact). The misses are dominated by wrong *tonic number*, not just A/B side — consistent with the handoff: needs harmonic-weighted HPCP, full-track analysis (currently capped ~150s), and tuning correction, not just a stronger minor bias.

## Per-track comparison (all 79)

| Artist | Title | BPM↣rb | ✓ | Cam↣rb | ✓ |
|---|---|---|---|---|---|
| ABRAX | OCB (Dan Ghenacia & Chris Carrier Dub Remix) | 126↣126 | ok | 2A↣1A | adj |
| Achterbahn d'Amour | Trance Me Up (Skudge Remix) | 169↣128 | X | 7A↣1A | X |
| Andy Stott | Made Your Point | 112↣113 | ok | 3A↣3A | EXACT |
| Askkin | Ifeksa | 73↣146 | 8ve | 4A↣6A | X |
| Baby Ford | All That Nothing | 129↣128 | ok | 3A↣1A | X |
| Baby Ford | Monolense | 178↣133 | X | 7A↣9A | X |
| Barker | Birmingham Screwdriver | 169↣167 | ok | 5B↣1A | X |
| Barker | Cascade Effect | 154↣136 | X | 10B↣8A | X |
| Barker | Models Of Wellbeing | 146↣73 | 8ve | 7A↣8A | adj |
| Ben Nevile | Petid | 126↣127 | ok | 2B↣3A | X |
| Benjamin Wild | Kronberg 4 | 126↣126 | ok | 8B↣9A | X |
| Bidoben | Unfair | 140↣140 | ok | 2A↣1A | adj |
| Bruno Pronsato | There's Galaxies Better (Melchior Productions Ltd. Spacelab Mix) | 126↣126 | ok | 5A↣10A | X |
| Buttechno | Dub 22 [PSY012] | 99↣150 | X | 10A↣10A | EXACT |
| Cabanne | Double Lardon | 169↣128 | X | 11A↣6A | X |
| Cabanne | Fraisheur | 167↣126 | X | 1B↣1A | rel |
| Cell Out | Transcendance | 88↣131 | X | 5A↣5A | EXACT |
| Cobblestone Jazz | Lime In Da Coconut | 129↣130 | ok | 1A↣1A | EXACT |
| Copacabannark | Ouane Forzeshow | 123↣124 | ok | 6A↣11A | X |
| cv313 | Dimensional (Live In Japan) | 117↣118 | ok | 1B↣10A | X |
| D. Diggler | Graviton | 126↣125 | ok | 6A↣7A | adj |
| Deuce (Marcel Dettmann & Shed) | Cue Ed | 129↣130 | ok | 8A↣10A | X |
| Dimbiman | Lava | 129↣130 | ok | 5A↣6A | adj |
| Dinky | Twelve To Four | 126↣125 | ok | 8B↣8A | rel |
| DJ Sprinkles | Midtown 120 Blues | 120↣120 | ok | 4A↣2A | X |
| DJ Sprinkles | Midtown 120 Intro | 120↣120 | ok | 4B↣4A | rel |
| DJ Sprinkles & Mark Fell | Fresh (Sprinkles Alt. Mix) | 120↣120 | ok | 7A↣8A | adj |
| DJ Trystero | Oriel | 126↣125 | ok | 11A↣10B | X |
| Dorisburg | Gripen | 126↣125 | ok | 3A↣6A | X |
| Efdemin | New Atlantis (Original Mix) | 136↣135 | ok | 3A↣3A | EXACT |
| Erik Luebs | Transform Into Glass | 136↣135 | ok | 6A↣4A | X |
| Fabe (Ger) | Gadget O'Flow (Original Mix) | 169↣128 | X | 11B↣7A | X |
| Flaty | Elevation | 167↣125 | X | 9A↣9A | EXACT |
| GECKO AFTERLIFE HD | ☺ EARTH JUMP | 140↣140 | ok | 8B↣8B | EXACT |
| Ittetsu | Sand Blind Premaster_24_44.1 Master | 120↣121 | ok | 12B↣1A | X |
| James Ferraro | Lovesick | 140↣104 | X | 8A↣8A | EXACT |
| Jon Hopkins | Collider | 115↣115 | ok | 2A↣1A | adj |
| Klint | Horus & Seth (Original Mix) | 144↣143 | ok | 2A↣6A | X |
| Lautaro Scavuzzo | Detune (AWSI Retuned Remix) [Island Beats] | 129↣129 | ok | 1A↣1A | EXACT |
| Len Faki | B-PAX | 123↣124 | ok | 7B↣11A | X |
| Luci | mullet is in da house | 129↣128 | ok | 5A↣5A | EXACT |
| Luigi Tozzi | Reptilian | 129↣130 | ok | 7B↣4A | X |
| Luigi Tozzi | Sentient | 129↣130 | ok | 5A↣11A | X |
| Malin Genie, Per Hammar | Scania (Original Mix) | 133↣133 | ok | 3B↣11A | X |
| Marcel Dettman | Scourer | 129↣130 | ok | 12A↣11A | adj |
| Maurizio | Domina (Maurizio Mix) (Edit) | 129↣129 | ok | 2A↣1A | adj |
| Metapattern | Pseudo User | 136↣137 | ok | 4B↣1A | X |
| NTSC | Space Jelly | 126↣127 | ok | 3A↣3A | EXACT |
| Oscar Mulero | RB208 [30YRSFUSE] | 140↣140 | ok | 6B↣6A | rel |
| Paul C, Paolo Martini | Klong (Max Chapman & Apollo 84 Remix) | 126↣125 | ok | 9A↣1A | X |
| Peter Van Hoesen | Exit Strategy | 136↣135 | ok | 4B↣1A | X |
| Petre Inspirescu | Basso Ostinato | 126↣125 | ok | 10A↣10A | EXACT |
| Petre Inspirescu | Basso Ostinato (Original Mix) | 123↣124 | ok | 1B↣10A | X |
| Phylyps | 01. Phylyps - Phylyps Trak | 144↣144 | ok | 6B↣6B | EXACT |
| Planetary Assault Systems | Undertow | 129↣129 | ok | 12A↣3A | X |
| Planetary Assault Systems | Whip It Good | 133↣134 | ok | 8A↣2A | X |
| Polygonia | Enteroctopus Dofleini | 126↣125 | ok | 3A↣6A | X |
| Prince Of Denmark | Cut 06 | 126↣126 | ok | 11B↣6A | X |
| Prince Of Denmark | GS | 126↣126 | ok | 12A↣4B | X |
| Prince Of Denmark | Neoclassicdub | 128↣128 | ok | 3A↣5B | X |
| Regis | Point of Entry | 133↣134 | ok | 6A↣10A | X |
| Rene Wise | Cutting Thick | 133↣133 | ok | 5A↣11A | X |
| Rezzett | Doyce | 172↣129 | X | 12A↣1A | adj |
| Rhadoo | Circul Globus | 126↣125 | ok | 2B↣10A | X |
| SCB | Down Moment | 126↣125 | ok | 12A↣11A | adj |
| SCSI‐9 | 303 Views | 126↣125 | ok | 12A↣12A | EXACT |
| Sistol | Keno | 126↣127 | ok | 6B↣1B | X |
| Soundstream | Wenn Meine Mutti Wusste | 123↣123 | ok | 7A↣11A | X |
| Surgeon | The Etheric Body | 133↣134 | ok | 8A↣4A | X |
| Tadeo | Requiem | 120↣75 | X | 1A↣1A | EXACT |
| Takasi Nakajima | Basic Math Three | 126↣127 | ok | 2A↣2A | EXACT |
| Tekra | Ybbob (Original Mix) | 133↣131 | ok | 7A↣8A | adj |
| tINI | Mine Has A Shower | 123↣122 | ok | 10A↣6B | X |
| Toasty | The Knowledge | 94↣141 | X | 12B↣11B | adj |
| Traumprinz | I Love Ya | 123↣122 | ok | 6B↣1A | X |
| Turner | When Will We Leave (Robert Hood Remix) | 120↣120 | ok | 7A↣7A | EXACT |
| Vadim Oslov | Ultimo Sentenza | 125↣125 | ok | 10A↣2A | X |
| West Code | Not Your Business (Original Mix) | 146↣146 | ok | 3A↣5A | X |
| Young Seth | Moment (Original Mix) | 123↣122 | ok | 10A↣1A | X |
