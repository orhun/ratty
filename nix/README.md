# Nix Packaging

This flake provides a Nix package for [Ratty](https://github.com/orhun/ratty), a GPU-rendered terminal emulator with inline 3D graphics.

## Supported Systems

- `x86_64-linux`
- `aarch64-linux`
- `x86_64-darwin`
- `aarch64-darwin`

## Quick Start

### Direct usage

```bash
# Run directly
nix run github:orhun/ratty

# Install to profile
nix profile install github:orhun/ratty
```

### As a flake input

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    ratty.url = "github:orhun/ratty";
  };

  outputs = { nixpkgs, ratty, ... }: {
    # Use in your configuration
  };
}
```

## NixOS System Configuration

Add ratty to your system packages with optional declarative configuration:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    ratty.url = "github:orhun/ratty";
  };

  outputs = { nixpkgs, ratty, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ratty.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

```nix
# configuration.nix
{
  programs.ratty = {
    enable = true;
    settings = {
      window = {
        opacity = 0.9;
        width = 1200;
        height = 800;
      };
      shell = {
        program = "zsh";
      };
      font = {
        family = "JetBrains Mono";
        size = 14;
      };
    };
  };
}
```

This will:

- Install the Ratty package
- Write configuration to `/etc/ratty/ratty.toml` (only when `settings` is non-empty)
- Wrap the binary to use `--config-file /etc/ratty/ratty.toml` (only when `settings` is non-empty)

### GPU Backend Selection

On systems with multiple GPUs or where the default Vulkan device creation fails
(e.g. NVIDIA 580.x drivers reporting unsupported features), set `gpuBackend` and
`gpuAdapter` to control wgpu device selection:

```nix
{
  programs.ratty = {
    enable = true;
    gpuBackend = "vulkan";    # or "gl" / "gles"
    gpuAdapter = "RTX 3060";  # substring match against adapter name
  };
}
```

When set, the NixOS module wraps the binary with `WGPU_BACKEND` and
`WGPU_ADAPTER_NAME` environment variables. When both `settings` and GPU options
are set, a single wrapper applies all flags.

## Home Manager Configuration

For user-level configuration without root:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager";
    ratty.url = "github:orhun/ratty";
  };

  outputs = { nixpkgs, home-manager, ratty, ... }: {
    homeConfigurations.myuser = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        ratty.homeManagerModules.default
        ./home.nix
      ];
    };
  };
}
```

```nix
# home.nix
{
  programs.ratty = {
    enable = true;
    settings = {
      window = {
        opacity = 0.85;
      };
      shell = {
        program = "fish";
      };
      theme = {
        foreground = "#c0caf5";
        background = "#1a1b26";
      };
    };
  };
}
```

This will:

- Install the Ratty package to your user profile
- Write configuration to `$XDG_CONFIG_HOME/ratty/ratty.toml` (typically `~/.config/ratty/ratty.toml`) (only when `settings` is non-empty)
- Set `WGPU_BACKEND` and `WGPU_ADAPTER_NAME` in the user session when GPU options are configured
- Ratty discovers this path automatically

### GPU Backend Selection (Home Manager)

Same options as NixOS, but applied via `home.sessionVariables` instead of a
binary wrapper:

```nix
{
  programs.ratty = {
    enable = true;
    gpuBackend = "vulkan";
    gpuAdapter = "RTX 3060";
  };
}
```

## Module Options

Both `nixosModules.default` and `homeManagerModules.default` expose:

| Option                      | Type         | Default                        | Description                                                                              |
| --------------------------- | ------------ | ------------------------------ | ---------------------------------------------------------------------------------------- |
| `programs.ratty.enable`     | bool         | `false`                        | Enable Ratty installation                                                                |
| `programs.ratty.package`    | package      | `self.packages.<system>.ratty` | The Ratty package to use                                                                 |
| `programs.ratty.settings`   | attrset      | `{}`                           | Configuration written to `ratty.toml`                                                    |
| `programs.ratty.gpuBackend` | null or enum | `null`                         | Force wgpu backend: `"vulkan"`, `"gl"`, or `"gles"`. null = auto-detect                  |
| `programs.ratty.gpuAdapter` | null or str  | `null`                         | Substring match to select a specific GPU adapter (e.g. `"RTX 3060"`). null = auto-detect |

## Package Architecture

```
flake.nix          — Orchestration, modules, devShell
nix/default.nix    — Standalone package (upstreamable to nixpkgs)
```

The package definition in `nix/default.nix` is designed to be upstreamed to nixpkgs as `pkgs/by-name/ra/ratty/package.nix`. It takes only standard nixpkgs arguments — no flake-specific constructs.

## Development

```bash
# Enter dev shell
nix develop

# Build package
nix build

# Run checks (build + tests)
nix flake check
```

## Maintainer

- DarthPJB <darthpjb@gmail.com>
