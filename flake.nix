# `nix run` (the game; it takes the converted assets from --assets / OPENKUMITE_ASSETS as ever: none are packaged),
# `nix develop` (the toolchain check.sh and web/build.sh need), `nix build` (openkumite and serve in result/bin).
{
  description = "OpenKumite, a 3D fighting game engine that plays game data converted by kumite_converter";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      # Linux only: macOS has its DMG (packaging/macos)
      forAll = f: lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system: f nixpkgs.legacyPackages.${system});
      # what winit and wgpu open when the game starts (dlopen): the GPU's loaders, Wayland, X11
      runtimeLibs = pkgs: with pkgs; [ vulkan-loader libGL wayland libxkbcommon libX11 libXcursor libXi libXrandr libxcb ];
      openkumite = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "openkumite";
        version = (lib.importTOML ./Cargo.toml).workspace.package.version;
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "-p" "openkumite" "-p" "serve" ];
        nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
        # what the binary links: ALSA (sound), libudev (gamepads)
        buildInputs = [ pkgs.alsa-lib pkgs.udev ];
        postFixup = ''
          wrapProgram $out/bin/openkumite --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath (runtimeLibs pkgs)}
        '';
        meta = {
          description = "Open-source 3D fighting game engine";
          homepage = "https://github.com/openkumite/openkumite";
          license = lib.licenses.mit;
          mainProgram = "openkumite";
        };
      };
    in {
      packages = forAll (pkgs: { default = openkumite pkgs; });

      apps = forAll (pkgs: {
        default = { type = "app"; program = lib.getExe (openkumite pkgs); meta.description = "OpenKumite"; };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (openkumite pkgs) ];
          # (lld: nixpkgs' rustc links wasm32 with it)
          packages = with pkgs; [ clippy rustfmt cargo-deny lld ]
            # web/build.sh's wasm-bindgen (it installs its own where nixpkgs has not this version)
            ++ lib.optional (pkgs ? wasm-bindgen-cli_0_2_129) wasm-bindgen-cli_0_2_129;
          LD_LIBRARY_PATH = lib.makeLibraryPath (runtimeLibs pkgs);
          # (where web/build.sh looks for it first, as pages.yml links it)
          shellHook = ''
            if command -v wasm-bindgen >/dev/null; then
              mkdir -p target/tools/bin && ln -sf "$(command -v wasm-bindgen)" target/tools/bin/wasm-bindgen
            fi
          '';
        };
      });
    };
}
