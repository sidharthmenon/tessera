## Tessera

Controls the Tessera media server: GO, cues, playback and master volume, with live feedback of
what is in Preview and on Program.

### Setup

1. In Tessera, click **Remote** in the status bar and tick **Remote control (Companion)**. The menu
   shows this computer's address, the port (7410 unless changed) and the access key (**Copy**).
2. In this connection: **Host** = that address (127.0.0.1 if Companion runs on the same computer),
   **Port**, **Access key**.
3. The connection turns green once connected. *Authentication failure* means the key is wrong (it
   changes if someone presses **New** in Tessera).

### Actions

| Action | What it does |
|---|---|
| GO | Preview to Program, with Preview's transition |
| Load cue into Preview | Cue *n* of group *g* (group 1 = the first tab, numbers as on the cue tiles) |
| Fire cue | Load + GO in one press |
| Load the next / previous cue | As the → key in Tessera |
| Playback | Program or Preview: play, pause, stop (back to the first frame), restart |
| Master volume: set / up/down | 0–100 |

GO, Fire and Program stop/restart act at once: there is no confirmation from Companion.

### Feedbacks

- **Cue is in Preview** (green) and **Cue is on Program** (red), for group *g*, cue *n*.
- **Preview was edited**: Preview differs from the cue it was loaded from.
- **Engine connected**.

### Variables

`$(tessera:show)`, `$(tessera:engine)`, `$(tessera:volume)`, `$(tessera:pvw_cue)`,
`$(tessera:pvw_name)`, `$(tessera:pgm_cue)`, `$(tessera:pgm_name)`, `$(tessera:pgm_clip)`,
`$(tessera:pgm_elapsed)`, `$(tessera:pgm_remaining)` (the Program clip that ends last, m:ss), and
each cue's name as `$(tessera:cue_<group>_<cue>)`, e.g. `$(tessera:cue_1_3)`.

### Presets

- **Show**: GO, Next, Previous, and an on-air display (cue name and time left).
- **Playback**: Program and Preview play/pause/stop/restart.
- **Volume**: up/down 5.
- **Cues: *group***: one button per cue (its number and name; press to load; green in Preview,
  red on Program). They follow the show: added cues appear as presets after the next update.
