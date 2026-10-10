# Standalone package definition for ratty.
# Designed to be upstreamed to nixpkgs as pkgs/by-name/ra/ratty/package.nix.
# Takes only standard nixpkgs arguments — no flake-specific constructs.
{
  lib,
  stdenv,
  craneLib,
  pkg-config,
  fontconfig,
  udev,
  wayland,
  libxkbcommon,
  libxcb,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  libxext,
  vulkan-loader,
  mesa,
  bash,
  makeWrapper,
  copyDesktopItems,
  makeDesktopItem,
  # Darwin frameworks — passed via callPackage override from flake
  darwinFrameworks ? [ ],
}:

let
  runtimeLibraryPath = lib.makeLibraryPath (
    lib.optionals stdenv.isLinux [
      vulkan-loader
      mesa
      fontconfig
      libxkbcommon
      libx11
      libxcb
      libxcursor
      libxi
      libxrandr
      libxext
    ]
  );

  # Shell script injected into the wrapper's --run argument.
  # Defined here to avoid nested ''…'' string issues in postInstall.
  runScript = ''
    for _arg in "$@"; do
      if [ "$_arg" = "-e" ] || [ "$_arg" = "--command" ]; then
        exec ${placeholder "out"}/bin/.ratty-env-wrapped "$@"
      fi
    done
    exec ${placeholder "out"}/bin/.ratty-env-wrapped -e "${bash}/bin/bash" "$@"
  '';

  # Common arguments shared between buildDepsOnly and buildPackage.
  # buildDepsOnly uses a filtered source (only cargo files) to maximize
  # cache hit rate — dependency hashes don't change when assets/docs change.
  commonArgs = {
    pname = "ratty";
    version = "0.4.2";

    src = craneLib.cleanCargoSource ../.;

    nativeBuildInputs = [
      pkg-config
      makeWrapper
      copyDesktopItems
    ];

    buildInputs =
      lib.optionals stdenv.isLinux [
        fontconfig
        udev
        wayland
        libxkbcommon
        libxcb
        libx11
        libxcursor
        libxi
        libxrandr
        libxext
        vulkan-loader
        mesa
      ]
      ++ darwinFrameworks;

    cargoLock = ../Cargo.lock;
  };

  # Build only the dependencies — this is the key caching layer.
  # Subsequent builds reuse these artifacts as long as Cargo.lock
  # and dependency code remain unchanged.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

in
# Build the full package, reusing cached dependency artifacts.
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;

    # The full source (including assets, config, website) is needed for
    # postInstall below.  buildDepsOnly already handled the filtered source.
    src = ../.;

    desktopItems = [
      (makeDesktopItem {
        name = "ratty";
        desktopName = "Ratty";
        comment = "A GPU-rendered terminal emulator with inline 3D graphics";
        exec = "ratty";
        terminal = false;
        categories = [
          "System"
          "TerminalEmulator"
          "Utility"
        ];
        icon = "ratty";
      })
    ];

    # Assets are embedded at compile time via rust-embed.
    # Copy them to $out/share for reference and custom model discovery fallback.
    postInstall = ''
      # Step 1: Copy assets
      mkdir -p $out/share/ratty
      cp -r assets/objects $out/share/ratty/
      install -Dm644 config/ratty.toml $out/share/ratty/ratty.toml
      install -Dm644 website/assets/images/ratty-logo.png \
        $out/share/icons/hicolor/512x512/apps/ratty.png

      # Step 2: wrapProgram for env var management
      wrapProgram $out/bin/ratty \
        --set-default SHELL '${bash}/bin/bash' \
        --set-default WGPU_SETTINGS_PRIO 'webgpu' \
        --prefix LD_LIBRARY_PATH : '${runtimeLibraryPath}' \
        ${lib.optionalString stdenv.isDarwin ''
          --prefix DYLD_LIBRARY_PATH : '${runtimeLibraryPath}' \
          --prefix DYLD_FALLBACK_LIBRARY_PATH : '${runtimeLibraryPath}' \
        ''}

      # Step 3: Thin wrapper for conditional -e "$SHELL" injection.
      # The --run script is defined as a separate Nix string to avoid
      # the nested double-tick problem (Nix does not support nested double-tick strings).
      mv $out/bin/ratty $out/bin/.ratty-env-wrapped
      makeWrapper $out/bin/.ratty-env-wrapped $out/bin/ratty \
        --run '${runScript}'
    '';

    meta = {
      description = "GPU-rendered terminal emulator with inline 3D graphics";
      homepage = "https://github.com/orhun/ratty";
      license = lib.licenses.mit;
      maintainers = [ "daniejbolt" ];
      mainProgram = "ratty";
      platforms = lib.platforms.unix;
    };
  }
)
