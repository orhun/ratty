{
  description = "Ratty: a GPU-rendered terminal emulator with inline 3D graphics";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem = nixpkgs.lib.genAttrs supportedSystems;

      # Shared module options — used by both HM and NixOS modules
      rattyOptions =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.ratty;
          tomlFormat = pkgs.formats.toml { };
        in
        {
          options.programs.ratty = {
            enable = lib.mkEnableOption "Ratty, a GPU-rendered terminal emulator";

            package = lib.mkOption {
              type = lib.types.package;
              default = pkgs.ratty;
              defaultText = lib.literalExpression "pkgs.ratty";
              description = "The ratty package to install.";
            };

            settings = lib.mkOption {
              type = tomlFormat.type;
              default = { };
              description = "Ratty configuration (ratty.toml).";
              example = lib.literalExpression ''
                {
                  window = {
                    opacity = 0.8;
                  };
                  shell = {
                    program = "bash";
                  };
                }
              '';
            };

            gpuBackend = lib.mkOption {
              type = lib.types.nullOr (
                lib.types.enum (
                  if pkgs.stdenv.isDarwin then
                    [
                      "metal"
                      "gl"
                      "gles"
                    ]
                  else
                    [
                      "vulkan"
                      "gl"
                      "gles"
                    ]
                )
              );
              default = null;
              description = ''
                Force the wgpu backend.
                Set to null (default) for auto-detection.
                Useful when the Vulkan ICD loader selects an incompatible driver
                or when running in headless/VNC environments where OpenGL is preferred.
              '';
              example = "vulkan";
            };

            gpuAdapter = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              description = ''
                Substring match to select a specific GPU adapter name.
                Set to null (default) to use the system default.
                Useful on multi-GPU systems (e.g. integrated + discrete)
                or when wgpu picks the wrong adapter.
              '';
              example = "RTX 3060";
            };
            defaultShell = lib.mkOption {
              type = lib.types.nullOr lib.types.package;
              default = null;
              description = ''
                Default shell to use when no -e/--command flag is passed.
                Set to null to use the package's built-in default (bash).
              '';
              example = lib.literalExpression "pkgs.zsh";
            };
          };
        };
    in
    {
      packages = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          craneLib = crane.mkLib pkgs;
        in
        {
          ratty = pkgs.callPackage ./nix/default.nix {
            inherit craneLib;
            # Pass Darwin frameworks when on Darwin
            darwinFrameworks = pkgs.lib.optionals pkgs.stdenv.isDarwin (
              with pkgs.darwin.apple_sdk.frameworks;
              [
                Cocoa
                CoreFoundation
                CoreGraphics
                CoreText
                CoreVideo
                Metal
                QuartzCore
              ]
            );
          };
          default = self.packages.${system}.ratty;
        }
      );

      devShells = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];
            packages = with pkgs; [
              rust-analyzer
              cargo
              clippy
              rustfmt
            ];
          };
        }
      );

      formatter = forEachSystem (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);

      # Overlay — adds pkgs.ratty for use with the NixOS/HM modules.
      overlays.default = final: prev: {
        ratty = self.packages.${final.stdenv.hostPlatform.system}.ratty;
      };

      checks = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          # Build + run tests (cargo test via cargoCheckHook)
          ratty = self.packages.${system}.ratty;
        }
      );

      # Home Manager module — declarative user-level config
      #
      # Usage in home.nix:
      #   programs.ratty = {
      #     enable = true;
      #     settings = {
      #       window = { opacity = 0.9; };
      #       shell = { program = "zsh"; };
      #     };
      #     # Optional: force wgpu backend / adapter selection
      #     gpuBackend = "vulkan";   # "vulkan" | "gl" | "gles"
      #     gpuAdapter = "RTX 3060"; # substring match
      #   };
      #
      # Config is written to $XDG_CONFIG_HOME/ratty/ratty.toml
      # GPU env vars are set via home.sessionVariables.
      # ratty discovers the config path automatically.
      homeManagerModules.default =
        args@{
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.ratty;
          tomlFormat = pkgs.formats.toml { };
          opts = rattyOptions args;
        in
        {
          inherit (opts) options;
          config = lib.mkIf cfg.enable {
            assertions = [
              {
                assertion = cfg.package != null;
                message = "programs.ratty.package must not be null when programs.ratty.enable is true";
              }
            ];
            home.packages = [ cfg.package ];
            xdg.configFile."ratty/ratty.toml" = lib.mkIf (cfg.settings != { }) {
              source = tomlFormat.generate "ratty.toml" cfg.settings;
            };
            home.sessionVariables = lib.mkMerge [
              (lib.mkIf (cfg.gpuBackend != null) { WGPU_BACKEND = cfg.gpuBackend; })
              (lib.mkIf (cfg.gpuAdapter != null) { WGPU_ADAPTER_NAME = cfg.gpuAdapter; })
              (lib.mkIf (cfg.defaultShell != null) { SHELL = lib.getExe cfg.defaultShell; })
            ];
          };
        };

      # NixOS module — declarative system-level config
      #
      # Usage in configuration.nix:
      #   programs.ratty = {
      #     enable = true;
      #     settings = {
      #       window = { opacity = 0.9; };
      #       shell = { program = "zsh"; };
      #     };
      #     # Optional: force wgpu backend / adapter selection
      #     gpuBackend = "vulkan";   # "vulkan" | "gl" | "gles"
      #     gpuAdapter = "RTX 3060"; # substring match
      #   };
      #
      # Config is written to /etc/ratty/ratty.toml
      # Binary is wrapped with --config-file and GPU env vars when set.
      nixosModules.default =
        args@{
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.ratty;
          tomlFormat = pkgs.formats.toml { };
          opts = rattyOptions args;
        in
        {
          inherit (opts) options;
          config = lib.mkIf cfg.enable {
            assertions = [
              {
                assertion = cfg.package != null;
                message = "programs.ratty.package must not be null when programs.ratty.enable is true";
              }
            ];
            environment.systemPackages = [
              (
                let
                  hasSettings = cfg.settings != { };
                  hasGpuOpts = cfg.gpuBackend != null || cfg.gpuAdapter != null;
                in
                if !(hasSettings || hasGpuOpts) then
                  cfg.package
                else
                  pkgs.symlinkJoin {
                    name = "ratty-system-wrapped";
                    paths = [ cfg.package ];
                    nativeBuildInputs = [ pkgs.makeWrapper ];
                    postBuild = ''
                      rm -f $out/bin/ratty
                      makeWrapper ${lib.getExe cfg.package} $out/bin/ratty \
                        ${lib.optionalString hasSettings "--add-flags \"--config-file /etc/ratty/ratty.toml\""} \
                        ${lib.optionalString (cfg.gpuBackend != null) "--set WGPU_BACKEND '${cfg.gpuBackend}'"} \
                        ${lib.optionalString (cfg.gpuAdapter != null) "--set WGPU_ADAPTER_NAME '${cfg.gpuAdapter}'"} \
                        ${lib.optionalString (
                          cfg.defaultShell != null
                        ) "--set-default SHELL '${lib.getExe cfg.defaultShell}'"}
                    '';
                  }
              )
            ];

            environment.etc."ratty/ratty.toml" = lib.mkIf (cfg.settings != { }) {
              source = tomlFormat.generate "ratty.toml" cfg.settings;
            };
          };
        };
    };
}
