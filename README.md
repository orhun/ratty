# ratty 🐁

![Built with Ratatui badge](https://img.shields.io/badge/Built_With-Ratatui-000?logo=ratatui&logoColor=fff&labelColor=000&color=fff)

Under construction ⚠️ Cheese ahead! 🧀

<div>
  <video src="https://github.com/user-attachments/assets/ebe4dc54-ec60-4399-a63e-0d04b3013433" alt="Ratty Demo"/>
</div>

## Features

- Spinning rat cursor ([customizable](#changing-the-cursor))
- Traditional 2D and [new 3D mode](#3d-mode)!
- [Inline 3D objects](#inline-3d-objects)
- [GPU-backed text rendering](#rendering-pipeline)
- Image support (via Kitty Graphics Protocol >:\()

### 3D mode

Ever wondered what's _behind_ the terminal? Press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Enter</kbd>!

<div>
  <video width="80%" src="https://github.com/user-attachments/assets/173971cc-b6bb-4897-857a-5db8e3c9e161"/>
</div>

## Installation

Requirements:

- Rust toolchain with Cargo
- A GPU / graphics stack supported by Bevy and wgpu

### From source

```bash
cargo install --git https://github.com/orhun/ratty
```

## Configuration

The default configuration file is available in [`config/ratty.toml`](config/ratty.toml).

You can copy this file to `$HOME/.config/ratty/ratty.toml` and customize it.

### Changing the cursor

```toml
[cursor.model]
path = "CairoSpinyMouse.obj"
scale_factor = 6.0
brightness = 0.5
x_offset = 0.5
plane_offset = 18.0
visible = true

[cursor.animation]
spin_speed = 1.4
bob_speed = 2.2
bob_amplitude = 0.08
```

For [`cursor.model.path`](config/ratty.toml), Ratty supports both `.obj` and `.glb` assets.

Other useful cursor fields are:

- `scale_factor`: scales the model relative to the terminal cell size
- `brightness`: adjusts the cursor material brightness
- `x_offset`: shifts the cursor model horizontally inside the cell
- `plane_offset`: pushes the cursor away from the warped terminal surface in 3D mode
- `visible`: show the custom 3D cursor model instead of only the terminal cursor

## Key Bindings

| Key                                             | Action              |
| ----------------------------------------------- | ------------------- |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>C</kbd>     | Copy selection      |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd>     | Paste clipboard     |
| <kbd>Ctrl</kbd>+<kbd>=</kbd>                    | Increase font size  |
| <kbd>Ctrl</kbd>+<kbd>-</kbd>                    | Decrease font size  |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Enter</kbd> | Toggle 2D / 3D mode |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Up</kbd>    | Increase warp       |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Down</kbd>  | Decrease warp       |

## Inline 3D objects

Ratty uses its own protocol, the [Ratty Graphics Protocol](protocols/graphics.md),
to place inline 3D objects in terminal space.

RGP supports:

- registering `.obj` and `.glb` assets by path
- placing them at terminal cell anchors
- animation, scale, color, depth and other attributes

There is a Ratatui widget called `ratatui-rgp` available in
[`widget/`](widget/) if you want to build your own terminal applications that involve inline 3D objects.

### Examples

#### [Big rat](widget/examples/big_rat.rs)

Places a single oversized rat directly in your terminal:

<div>
  <video width="80%" src="https://github.com/user-attachments/assets/e955d09a-d0eb-4bad-b3b2-fc1331f49646"/>
</div>

#### [Document](widget/examples/document.rs)

TempleOS-inspired document demo with editable text and embedded inline 3D objects:

<div>
  <video width="80%" src="https://github.com/user-attachments/assets/f3a085b0-9e34-4b6f-92fb-90eff9f11776"/>
</div>

#### [Draw](widget/examples/draw.rs)

Split-pane drawing demo with a 2D canvas on the left and a live 3D preview on the right:

<div>
  <video width="80%" src="https://github.com/user-attachments/assets/8b53515b-b887-4d03-a54c-7e7aa7ea128c"/>
</div>

## Architecture

### Rendering pipeline

The terminal surface currently uses [`ratatui`](https://github.com/ratatui/ratatui) for the UI buffer,
[`parley_ratatui`](https://github.com/gold-silver-copper/parley_ratatui) for text shaping/rendering
and [Bevy](https://bevyengine.org/) for scene presentation.

Current workflow:

1. Ratatui buffer on CPU
2. Parley/Vello renders on GPU
3. Read back RGBA to CPU
4. Copy into Bevy image
5. Bevy presents that image in 2D and 3D

Terminal drawing is GPU-rendered through Parley/Vello, but the main terminal
image still crosses back through CPU memory before Bevy presents it. This is a
GPU-powered bridge, not a fully GPU-resident shared-texture path.

If the project later moves to a fully GPU-resident path, that will require a
dedicated Bevy render integration that renders into a Bevy-owned texture on
Bevy's render-world device instead of using the current readback bridge.

## Endorsements

- _"This is like a legitimately cool project but also I just spent like 20 minutes adjusting the config for the rat spinning to see him spin faster and more erratically and it cracked me up"_ - [@vimlena.com](https://bsky.app/profile/vimlena.com/post/3mkoshbzpvs2y)

<div>

<video width="80%" src="https://github.com/user-attachments/assets/76446086-0432-4b67-b768-1fe31134a2c1">

</div>

- _"These kinds of experiments are where creativity is born."_ - [@Coko7](https://github.com/Coko7)

- _"No comments. Just support."_ - [@Raphamorim](https://github.com/raphamorim/) (creator of Rio terminal)

## License

All code is licensed under <a href="LICENSE">The MIT License</a>.

<br>

<small>
The author does not have a rat under the hat 🐁
</small>
