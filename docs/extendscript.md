# Running `.jsx` scripts

`vectorcraft-extendscript` runs scripts written for the reference app's scripting language
(`.jsx`) against a VectorCraft session. A JavaScript engine (`boa_engine`) runs the language; the
crate defines the objects a script reaches and turns every read and change into the engine's
document summary and commands, so a script's edits go through undo, the journal and the guard.

```sh
vectorcraft-cli script tool.jsx --in drawing.svg --select-all --export out.png --save out.vectorcraft
```

`vectorcraft-cli script` prints each `alert()` and the script's last value as JSON lines. For
what a script needs before it runs, see File › Scripts › Check Script (`vectorcraft-scriptcheck`).

## Clean room

Nothing here reads, runs or copies the reference app. The names (`app`, `layers`, `pageItems`,
`geometricBounds`, …) come from its public scripting reference, and what a script sees beyond
them is what VectorCraft does. Tests are written from scratch; scripts people own are run locally
and never committed.

## Coordinates

A script sees the page with y pointing up, the origin at the bottom-left corner of the first
artboard, and a rectangle as `[left, top, right, bottom]` (`geometricBounds`, `artboardRect`).
The public reference documents the bottom-left origin with y increasing upward for documents a
script works on, and does not state the order of a rectangle's values; the order is the one scripts
in use rely on. Lengths are points.

## What works

| Object | Supported |
|---|---|
| `app` | `documents`, `activeDocument`, `userInteractionLevel`, `redraw()` |
| `Document` | `name`, `layers`, `pageItems`, `pathItems` (`add()`, `rectangle(top, left, width, height)`), `textFrames` (`add()`), `artboards` (`add([l, t, r, b])`), `selection` |
| `Layer`, groups, paths, text | `typename`, `name`, `visible`, `hidden`, `locked`, `layers`, `pageItems`, `geometricBounds`, `visibleBounds`, `width`, `height`, `position`, `top`, `left`, `translate()`, `rotate()`, `zOrder()`, `remove()` |
| Paint | `fillColor`, `strokeColor` (an `RGBColor`; its channels are live, so `item.strokeColor.red = 237` after `item.strokeColor = new RGBColor()` works), `strokeWidth`, `filled`, `stroked` |
| Paths | `setEntirePath([[x, y], …])` on a path from `pathItems.add()` |
| Text | `contents`, `textRange.characterAttributes.size` and `.fillColor`, `textRange.paragraphAttributes.justification` |
| Artboards | `name`, `artboardRect`, `getByName()` on collections |
| Globals | `alert`, `RGBColor`, `Justification`, `ZOrderMethod`, `UserInteractionLevel` |

## Not supported yet

`Window` and the rest of ScriptUI, `File` and `Folder`, `$.evalFile` and `#include`, CMYK and
other colour types, `resize()` and `duplicate()`, character fonts, `executeMenuCommand`,
`app.open` and `saveAs`, and E4X. A script that uses one stops with the engine's error, which
names the missing object. `setEntirePath` on a path that already has points, and `contents` or
`setEntirePath` before an object from `add()` has content, are errors too.
