# PBC authoring JSON

What `motion check`, `motion preview` and `motion render` accept, besides a `.pbc`.
One file describes a whole project. Write it, run `motion preview spec.json`, read
the JSON report and the PNG sheet, and repeat.

## Conventions
- Time is in **seconds** on the comp clock, including keyframe times.
- Pixels, with the origin at the comp's top-left and y going down.
- `layers[0]` draws **on top**, like layer 1 in AE.
- Rotation is in degrees, opacity is 0-100, scale is a percent `[100, 100]`.
- Colours are `"#rrggbb"` or `[r, g, b]` in 0..1.
- Always set `transform.position` on a layer. Shape coordinates are relative to it.
- The first composition is rendered unless `--comp <index>` is passed.

## Values
Any animatable property takes one of these:
- static: `5`, `[x, y]` or `"#ff0000"`
- keyframes: `{ "keyframes": [ { "time": 0, "value": [0, 0] }, { "time": 1, "value": [100, 0], "ease": "easeInOut" } ] }`
  - `interp` is `"linear"` (the default), `"bezier"` or `"hold"`.
  - `ease` is a preset string (any name other than `"linear"` gives a smooth 75% ease), or
    `{ "inSpeed", "inInfluence", "outSpeed", "outInfluence" }` (influence is 0-100).
- expression: `{ "value": 0, "expression": "time * 90" }`

## Shape
```
document     { compositions: [comp], assets?: [] }
comp         { name, width, height, duration, fps?=24, backgroundColor?="#000000", layers: [layer] }
layer        { name, type, startTime?=0, in?, out?, parent?: layerName, blendMode?: "Multiply" | "Screen" | "Add" | …,
               trackMatte?: { type: "Alpha"|"Alpha Inverted"|"Luma"|"Luma Inverted", target: layerName },
               transform?: { anchorPoint, position, xPosition, yPosition, scale, rotation,
                             xRotation, yRotation, zRotation, orientation, opacity },
               threeD?: bool, enabled?: bool, adjustment?: bool,
               shapes?: [shape],         // type "shape"
               text?: text,              // type "text"
               source?: compName,        // type "precomp"
               effects?: [effect] }
layer.type   shape | text | null | precomp | adjustment | camera | light
             (solid / footage / audio are accepted but NOT drawn yet; use a shape rect for a solid)
text         { string, font?="ArialMT", fontSize?=48, fillColor?, strokeColor?, strokeWidth?, tracking?,
               justification?: left|center|right, leading?, boxSize?: [w, h], applyFill?, applyStroke? }
shape        group   { items: [shape], transform?: { anchorPoint, position, scale, rotation, opacity } }
             rect    { size, position?, roundness? }       // position is the centre
             ellipse { size, position? }
             path    { path: { vertices: [[x,y]…], inTangents?, outTangents?, closed?=true } }
             fill    { color, opacity? }
             stroke  { color, strokeWidth, opacity?, lineCap?: butt|round|projecting,
                       lineJoin?: miter|round|bevel, dashes?: { dash, gap, offset } }
             trim    { start, end, offset? }               // 0-100, animates stroke draw-on
             every shape item may also carry "name"
effect       { matchName, props: { "<matchName>-<4-digit index>": value } }
```
Fills and strokes paint the paths that come before them in the same `items` / `shapes` list.

## Effects
| matchName | params (index: meaning) |
|---|---|
| ADBE Gaussian Blur 2 | 0001 blurriness |
| ADBE Drop Shadow | 0001 color, 0002 opacity 0-255, 0003 direction°, 0004 distance, 0005 softness |
| ADBE Fill | 0002 color, 0005 opacity % |
| ADBE Brightness & Contrast 2 | 0001 brightness, 0002 contrast |
| ADBE HUE SATURATION | 0004 hue°, 0005 saturation, 0006 lightness |
| ADBE Exposure2 | 0003 exposure, 0004 offset, 0005 gamma |
| ADBE Sharpen | 0001 amount |
| ADBE Noise | 0001 amount, 0002 color noise |
| ADBE Mosaic | 0001 horizontal, 0002 vertical blocks |
| ADBE Fractal Noise | 0004 contrast, 0005 brightness, 0010 scale, 0015 complexity, 0023 evolution, 0027 seed |
| ADBE Unmult | (none) |

Example: `{ "matchName": "ADBE Gaussian Blur 2", "props": { "ADBE Gaussian Blur 2-0001": 20 } }`

## Example
```json
{ "compositions": [{ "name": "Main", "width": 1920, "height": 1080, "fps": 30, "duration": 3,
  "backgroundColor": "#101218",
  "layers": [
    { "name": "Title", "type": "text", "text": { "string": "HELLO", "fontSize": 160, "fillColor": "#ffffff", "justification": "center" },
      "transform": { "position": [960, 600],
                     "opacity": { "keyframes": [{ "time": 0.3, "value": 0 }, { "time": 1, "value": 100, "ease": "easeOut" }] } } },
    { "name": "Bar", "type": "shape", "transform": { "position": [960, 660],
        "scale": { "keyframes": [{ "time": 0, "value": [0, 100] }, { "time": 0.8, "value": [100, 100], "ease": "easeInOut" }] } },
      "shapes": [{ "type": "rect", "size": [600, 12], "roundness": 6 }, { "type": "fill", "color": "#ff3b30" }] }
  ] }] }
```

## Commands
- `motion check <spec>` validates only. It prints the JSON report and exits 1 on any error.
- `motion preview <spec> [--frames 0,30,60 | --count 6] [--width 480] [--out sheet.png]`
  renders the chosen frames into one PNG grid, left to right and top to bottom.
  `report.frames` gives the frame shown in each cell.
- `motion render <spec> --out out.mp4 [--quality master] [--json]` writes the final file.

Every report has `ok`, `errors[]` and `warnings[]` (each `{path, message}`), plus
`dropped` (features the importer couldn't carry) and `notes` (things the renderer
couldn't draw, e.g. footage).
