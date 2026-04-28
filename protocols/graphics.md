# Ratty Graphics Protocol

Ratty Graphics Protocol (RGP) is a custom terminal protocol for inserting
3D objects into the terminal as first-class inline objects.

The goal is to attach a semantic graphics object to terminal cells,
so it becomes part of the terminal surface rather than an external overlay.

It is inspired by [TempleOS]-style inline document graphics ([DolDoc])
and by modern terminal extension work such as [Glyph Protocol],
but it is designed for Ratty's inline object layer and 3D renderer.

## Design Goals

- Support 3D assets directly.
- Anchor objects to terminal cell positions.
- Make graphics native terminal objects, not external overlays.
- Allow future interactive behavior such as clicking, macros and object state updates.

## Transport

Ratty Graphics Protocol uses [APC] (Application Program Command):

```text
ESC _ ratty;g;<verb>[;<key=value>...] ESC \
```

Where:

- `ratty` is the protocol namespace
- `g` means graphics
- [`<verb>`](#verbs) selects the operation
- additional fields are semicolon-separated `key=value` pairs

## Model

Ratty treats protocol objects as inline terminal objects.

Each object has:

- an object id
- an anchor cell
- a cell span
- a renderable payload
- optional metadata for future interaction

## Verbs

- `s` [support query](#1-support-query)
- `r` [register object asset](#2-register-object-asset)
- `p` [place object](#3-place-object)
- `d` [delete object](#4-delete-object)

### 1. Support Query

Used to detect protocol support and version.

Client sends:

```text
ESC _ ratty;g;s ESC \
```

Ratty replies:

```text
ESC _ ratty;g;s;v=1;fmt=obj|glb;path=1;anim=1;depth=1;color=1;brightness=1 ESC \
```

Fields:

- `v=1`: protocol version
- `fmt=glb`: `obj` and `glb` are supported in v1
- `path=1`: path-based object registration is supported
- `anim=1`: `animate=1` placement is supported
- `depth=1`: `depth=<f32>` placement is supported
- `color=1`: `color=<RRGGBB>` placement is supported
- `brightness=1`: `brightness=<f32>` placement is supported

If no reply arrives, the terminal does not support the protocol.

### 2. Register Object Asset

Registers a 3D object by id.

Client sends:

```text
ESC _ ratty;g;r;id=42;fmt=obj;path=CairoSpinyMouse.obj ESC \
```

This registers object `42` using an object asset.

#### Required fields

- `id`: object id chosen by the application
- `fmt`: payload format, `obj` or `glb` in v1
- `path`: object path known to Ratty

### 3. Place Object

Places a previously registered object into terminal cell space.

Client sends:

```text
ESC _ ratty;g;p;id=42;row=12;col=8;w=4;h=2;animate=1;scale=1.0;depth=2.5;color=ff8844;brightness=1.0 ESC \
```

Fields:

- `id`: registered object id
- `row`: anchor row at the center of the placement
- `col`: anchor column at the center of the placement
- `w`: width in terminal cells
- `h`: height in terminal cells
- `animate`: optional, `1` enables default animation
- `scale`: optional scale factor, defaults to `1.0`
- `depth`: optional z-offset, defaults to `0.0`
- `color`: optional RGB color as `RRGGBB`
- `brightness`: optional brightness multiplier, defaults to `1.0`

### 4. Delete Object

Deletes either a placement or an object.

Delete one object:

```text
ESC _ ratty;g;d;id=42 ESC \
```

Delete all Ratty graphics objects:

```text
ESC _ ratty;g;d ESC \
```

## Example Session

Register an embedded object path:

```text
ESC _ ratty;g;r;id=7;fmt=obj;path=CairoSpinyMouse.obj ESC \
```

Place it in the terminal at row 5, column 10, spanning 3×2 cells:

```text
ESC _ ratty;g;p;id=7;row=5;col=10;w=3;h=2;animate=1;scale=1.0;depth=1.5;color=7fd0ff;brightness=1.0 ESC \
```

Delete it:

```text
ESC _ ratty;g;d;id=7 ESC \
```

## Summary

Ratty Graphics Protocol is a terminal-native object protocol for 3D graphics.

Its key ideas are:

- register a renderable object
- place it in terminal cell space
- let Ratty render it as part of the terminal, including 3D mode

That is the foundation for "sprites on the command line" in Ratty, inspired by [TempleOS]-style
inline graphics but designed for modern terminal capabilities and 3D rendering.

[TempleOS]: https://templeos.org
[DolDoc]: https://tinkeros.github.io/WbTempleOS/Doc/DolDocOverview.html
[Glyph Protocol]: https://rapha.land/introducing-glyph-protocol-for-terminals/
[APC]: https://en.wikipedia.org/wiki/C0_and_C1_control_codes#C1_controls
