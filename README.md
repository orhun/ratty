# ratty 🐁

Under construction ⚠️ Cheese ahead! 🧀

<div>
<video src="https://github.com/user-attachments/assets/6e5e82fa-d9bd-4c30-a387-d86f0859280d" alt="Ratty Demo"/>
</div>

Features:

- Spinning rat cursor
- 2D and 3D terminal presentation modes
- Inline 3D object support via [Ratty Graphics Protocol (RGP)](protocols/graphics.md)
- Image support via Kitty Graphics Protocol
- GPU-backed text rendering with Ratatui + Parley/Vello [\*](#rendering-pipeline)

[**Sponsor the development of ratty!**](https://github.com/sponsors/orhun/)

## Rendering pipeline

The terminal surface currently uses [`ratatui`](https://github.com/ratatui/ratatui) for the UI buffer,
[`parley_ratatui`](https://github.com/gold-silver-copper/parley_ratatui) for text shaping/rendering
and [Bevy](https://bevyengine.org/) for scene presentation.

Current workflow:

1. Ratatui buffer on CPU
2. Parley/Vello renders on GPU
3. Read back RGBA to CPU
4. Copy into Bevy image
5. Bevy uses that texture on its side

What this means:

- Terminal drawing is GPU-rendered through Parley/Vello
- The Bevy integration is still a bridge, not a zero-copy shared-texture path
- The 2D sprite and 3D terminal plane both consume the same Bevy-side image

This is GPU-powered terminal rendering, but it is not fully GPU-resident.

If the project later moves to a fully GPU-resident path, that will require a
dedicated Bevy render integration that renders into a Bevy-owned texture on
Bevy's render-world device instead of using the current readback bridge.

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for more information.
