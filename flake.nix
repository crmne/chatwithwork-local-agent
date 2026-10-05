{
  description = "Chat with Work desktop app, terminal interface and background agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        # The pinned Nixpkgs has dropped Intel macOS; native packages remain universal.
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          cww = pkgs.rustPlatform.buildRustPackage {
            pname = "cww";
            version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
            src = self;

            # Registry dependencies come straight from Cargo.lock, so a lock
            # file update needs no vendor hash refresh.
            cargoLock.lockFile = ./Cargo.lock;
            # Git dependencies are pinned by the workspace lock file.
            cargoLock.allowBuiltinFetchGit = true;
            cargoBuildFlags = [ "--workspace" ];
            cargoTestFlags = [ "--workspace" ];
            nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.makeWrapper ]
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libicns ];

            # The end-to-end test binds a local port; the rest touches only
            # temporary directories.
            __darwinAllowLocalNetworking = true;

            postInstall = if pkgs.stdenv.hostPlatform.isLinux then ''
              install -Dm644 packaging/systemd/cww.service $out/lib/systemd/user/cww.service
              substituteInPlace $out/lib/systemd/user/cww.service \
                --replace-fail /usr/bin/cww $out/bin/cww \
                --replace-fail /usr/bin/mkdir ${pkgs.coreutils}/bin/mkdir
              install -Dm644 packaging/linux/cww-app.desktop $out/share/applications/cww-app.desktop
              substituteInPlace $out/share/applications/cww-app.desktop \
                --replace-fail Exec=cww-app Exec=$out/bin/cww-app
              install -Dm644 app/assets/mark.svg $out/share/icons/hicolor/scalable/apps/cww-app.svg
              wrapProgram $out/bin/cww-app --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (with pkgs; [
                libglvnd wayland libxkbcommon libX11 libXcursor libXi libXrandr
              ])}
            '' else ''
              app="$out/Applications/Chat with Work.app"
              mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
              substitute packaging/macos/Info.plist "$app/Contents/Info.plist" \
                --replace-fail __VERSION__ ${((pkgs.lib.importTOML ./Cargo.toml).package.version)}
              png2icns "$app/Contents/Resources/cww-app.icns" app/assets/icon-1024.png
              ln -s "$out/bin/cww" "$app/Contents/MacOS/cww"
              ln -s "$out/bin/cww-app" "$app/Contents/MacOS/cww-app"
            '';

            meta = {
              description = "Chat with Work desktop app, terminal interface and background agent";
              homepage = "https://github.com/crmne/chatwithwork-local-agent";
              license = with pkgs.lib.licenses; [
                mit
                asl20
              ];
              mainProgram = "cww";
              platforms = systems;
            };
          };
        in
        {
          default = cww;
          inherit cww;
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.cww ];
          packages = with pkgs; [
            rust-analyzer
            clippy
            rustfmt
            cargo-deny
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
