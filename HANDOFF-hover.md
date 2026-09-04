# Handoff: vinyl grid hover bugs

## Symptoms (reported by user, from a screenshot of a cover card)
1. Hovering the **dig** disc (green magnifier) makes the **play** disc disappear.
   The reverse works fine — hovering play keeps dig visible.
2. Hovering any of the card's overlay buttons **unhighlights the green outer
   border** that normally lights up while the cover is hovered.

## Where the code is
`crates/ordnung-gui/src/views.rs`, in `fn vinyl_grid` (starts ~line 1350).
This is the only place these cards are drawn — the green disc styling
(`from_rgb(120, 220, 150)` / `from_black_alpha(190)`) appears nowhere else in
the crate. The other `VinylGridAction::Dig` call site further down (~line 1650)
is a right-click context-menu item, not a card button; ignore it.

The card has four interactive pieces layered on one cover rect:
- the cover itself (`resp`, from `allocate_exact_size`)
- play disc, bottom-right (`hit` / `disc`)
- dig disc, to its left (`dig_hit` / `dig_rect`)
- "in your catalog" badge, top-right (`badge` / `badge_rect`), only when
  `!c.linked.is_empty()`

## Diagnosis
Each overlay button used to call `ui.interact(...)` at the point where it was
*painted*. egui hover state only exists once `interact` has run, so a widget
could only see the hover of widgets registered **before** it. That produced
exactly the asymmetry reported:

- play is painted first -> when it decided whether to show itself, `dig_hit`
  did not exist yet -> hovering dig hid play.
- dig is painted second -> it could already read `play_hovered` -> hovering
  play kept dig visible.
- the border is painted before all of them -> it could see none -> hovering any
  button dropped the border.

The discs sit *on top of* the cover, so pointing at one takes `resp.hovered()`
away from the cover. That is the underlying reason all three needed a shared
notion of "pointer is somewhere on this card".

## Change already applied (uncommitted, in the working tree)
All overlay hit areas were hoisted to just after the cover is allocated, before
anything paints, and collapsed into one flag:

```rust
let card_hovered = resp.hovered() || play_hovered || dig_hovered || badge_hovered;
```

- the border now tests `card_hovered` instead of `resp.hovered()`
- play now tests `card_hovered || playing_this`, and colors on `play_hovered`
- dig now tests `card_hovered`, and colors on `dig_hovered`
- the badge was folded in too (same defect, and the user's second report was
  about record buttons generally, not just the two discs)

`cargo check -p ordnung-gui` passes.

## !! Why the user still sees the bug — CHECK THIS FIRST
The change was almost certainly never running:

- `Ordnung.app/Contents/MacOS/Ordnung` is dated **Aug 28 18:46**, before the edit.
- `target/debug/ordnung-gui` and `target/release/ordnung-gui` **do not exist** —
  only `cargo check` had been run, which type-checks without producing a binary.

So verify against a fresh build before assuming the fix is wrong:

```
make run        # cargo run -p ordnung-gui, straight from source
# or
make app-only   # rebuild + sign the local bundle without touching /Applications
```

If it reproduces on a genuinely fresh build, then the diagnosis above is
incomplete — that is where real debugging starts.

## If it genuinely still reproduces
Things worth checking, roughly in order:
1. Confirm you are looking at the vinyl grid and not some other view with
   similar-looking cards. Ask the user which screen/tab the screenshot is from.
2. `egui::Sense::click()` on overlapping rects: a later `interact` on an
   overlapping area can steal hover from an earlier one. Confirm `play_hovered`
   and `dig_hovered` are both actually true when expected — paint a debug label
   or `dbg!` them per frame.
3. Check whether `interact_bg` / layer ordering or `ui.interact` id collisions
   (`ui.id().with(("vinyl-play", c.key))`) are involved when two cards share a key.
4. The `disc`/`dig_rect` geometry is computed from `rect`; verify the two rects
   don't overlap (dig sits at `disc.left() - D - 5.0`, D=30, so a 5px gap).

## Unrelated changes in the tree — DO NOT REVERT
`crates/ordnung-gui/src/vinyl_sheet.rs` is also modified. That is the user's own
pre-existing work on cover-cache fallback (keeping `cover_url` so a sheet that
outlives its list row still shows art). It has nothing to do with this bug.
